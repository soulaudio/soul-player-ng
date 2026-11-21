//! The player actor: two non-real-time threads around a `Player`.
//! - **pump**: owns the `Player` + sink, drains the command channel, services
//!   decoder opens, pumps one block per cycle, and emits throttled state.
//! - **loader**: resolves `id → path` against the current `Library` and opens
//!   the (large, `Send`) `OwnedDecoder` off the pump thread.
//!
//! Sink-generic via a factory run ON the pump thread, so a `!Send` device sink
//! (`CpalSink`) works and this crate never names `cpal`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use soul_audio_control::Player;
use soul_audio_core::{Deck, Progress, Quality, Sink};
use soul_audio_desktop::OwnedDecoder;
use soul_audio_engine::{Engine, Provided};

use crate::driver::{
    OutputConfig, OutputSwap, RgInfo, ServicePlayer, VolumeState, apply_command, apply_replay_gain,
    maybe_apply_output_change, maybe_switch_native_rate, project_queue, project_signal_path,
    project_tick, queue_for, rebuild_output_sink,
};
use crate::library::TrackSource;
use crate::loudness::LoudnessCache;
use crate::types::{Command, Event, PlayState, ReplayGainMode};

/// Golden-ratio odd constant; advanced per shuffle to vary the permutation seed
/// without an RNG dependency (deterministic across runs).
const SEED_STEP: u64 = 0x9E37_79B9_7F4A_7C15;

/// Handle to the running player actor. Dropping it stops the threads.
pub struct AudioPlayer {
    cmd_tx: Option<Sender<Command>>,
    library: Arc<Mutex<Arc<dyn TrackSource>>>,
    pump: Option<JoinHandle<()>>,
}

impl AudioPlayer {
    /// Spawn the actor. `make_sink(&OutputConfig)` runs on the pump thread (so `K`
    /// need not be `Send`) and builds a sink for a given output config: once at
    /// startup from `output_desired`, and again whenever the host changes the device
    /// or exclusive mode live (the pump rebuilds the sink in place — per-track
    /// native-rate switching still reopens that sink via `Sink::reopen`).
    /// `make_park` builds a device-FREE placeholder sink (same `K`): the pump parks on
    /// it to drop the old sink (freeing its device) before `make_sink` opens the new
    /// one — release-then-acquire, required so a same-device exclusive switch doesn't
    /// hit device-in-use. `output_desired` is the shared, host-mutable desired output
    /// config the pump watches. `on_event` is called from the pump thread on every
    /// emitted event. `library` is the initial `id → path` map (swap later via
    /// [`set_library`](Self::set_library)).
    pub fn spawn<K, Mk, Pk, Ev>(
        make_sink: Mk,
        make_park: Pk,
        output_desired: Arc<Mutex<OutputConfig>>,
        library: Arc<dyn TrackSource>,
        on_event: Ev,
    ) -> Self
    where
        K: Sink + 'static,
        Mk: Fn(&OutputConfig) -> anyhow::Result<K> + Send + 'static,
        Pk: Fn() -> K + Send + 'static,
        Ev: Fn(Event) + Send + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let library = Arc::new(Mutex::new(library));
        let loader_lib = Arc::clone(&library);

        // 64 MiB: the pump owns a `Player` holding several `OwnedDecoder`
        // preload slots, and `OwnedDecoder` embeds the large by-value
        // `AnyDecoder` enum (~320 KiB — sized by its multichannel AAC / surround
        // Opus variants). Debug-build moves through the nested Player/Engine/Deck
        // construction multiply that well past the former 16 MiB; 64 MiB leaves
        // headroom as decoder variants grow. (Release move-elision needs far
        // less; see the `anydecoder-size-stack` note.)
        let pump = thread::Builder::new()
            .name("player-pump".into())
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                pump_thread(
                    make_sink,
                    make_park,
                    output_desired,
                    cmd_rx,
                    loader_lib,
                    on_event,
                )
            })
            .expect("spawn pump thread");

        Self {
            cmd_tx: Some(cmd_tx),
            library,
            pump: Some(pump),
        }
    }

    /// Queue a command (fire-and-forget). No-op if the actor has stopped.
    pub fn send(&self, cmd: Command) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(cmd);
        }
    }

    /// Replace the library used by the loader to resolve `id → path`.
    pub fn set_library(&self, library: Arc<dyn TrackSource>) {
        *self.library.lock().expect("library mutex") = library;
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        // Disconnect the command channel FIRST (drop cmd_tx) so the pump loop
        // observes `Disconnected`, then joins. The pump only exits when the
        // channel is disconnected, so joining before dropping cmd_tx deadlocks.
        drop(self.cmd_tx.take());
        if let Some(h) = self.pump.take() {
            let _ = h.join();
        }
    }
}

/// The loader thread: `id → (ReplayGain tags, OwnedDecoder)`, sent back to the
/// pump. The RG tags are read here, on the loader thread, so the pump's hot loop
/// never touches the filesystem for them; they ride the same message as the
/// decoder, keyed to the id, and the pump banks them for when the track becomes
/// current (see [`pump_thread`]).
///
/// The decoder itself opens via [`TrackSource::open_decoder`], NOT a direct
/// `path_of` + `open_track` — a local `Library` resolves that straight to
/// `open_track` (the trait's default), but a remote source (the host's
/// `RemoteLibrary`) overrides it to STREAM over HTTP Range instead of
/// downloading the whole file first. RG tags still go through `path_of` (see
/// [`read_rg_tags`]): a local path reads tags straight off disk, and a remote
/// one resolves through the existing download-cache fetch — the same tag
/// source remote playback already used before streaming, kept as-is so RG
/// never regresses for either kind of track.
fn loader_thread(
    req_rx: Receiver<u32>,
    res_tx: Sender<(u32, RgInfo, anyhow::Result<OwnedDecoder>)>,
    library: Arc<Mutex<Arc<dyn TrackSource>>>,
) {
    for id in req_rx {
        let (rg, opened) = {
            let lib = library.lock().expect("library mutex").clone();
            let rg = lib
                .path_of(id)
                .map_or_else(RgInfo::default, |path| read_rg_tags(&path));
            (rg, lib.open_decoder(id))
        };
        if res_tx.send((id, rg, opened)).is_err() {
            break; // pump gone
        }
    }
}

/// Read a track's ReplayGain tags into an [`RgInfo`] (best-effort — an
/// unopenable / untagged / malformed file yields the all-`None` default, which
/// the pump treats as "no normalization data, preamp only"). Mirrors
/// [`crate::art::cover_bytes`]'s `read_owned`-over-a-`File` path.
fn read_rg_tags(path: &Path) -> RgInfo {
    use soul_audio_tags::read_owned;
    let Ok(file) = std::fs::File::open(path) else {
        return RgInfo::default();
    };
    let mut src = embedded_io_adapters::std::FromStd::new(file);
    match read_owned(&mut src) {
        Ok((tags, _facts)) => RgInfo {
            track_gain_db: tags.track_gain_db,
            album_gain_db: tags.album_gain_db,
        },
        Err(_) => RgInfo::default(),
    }
}

fn pump_thread<K, Mk, Pk, Ev>(
    make_sink: Mk,
    make_park: Pk,
    output_desired: Arc<Mutex<OutputConfig>>,
    cmd_rx: Receiver<Command>,
    library: Arc<Mutex<Arc<dyn TrackSource>>>,
    on_event: Ev,
) where
    K: Sink + 'static,
    Mk: Fn(&OutputConfig) -> anyhow::Result<K>,
    Pk: Fn() -> K,
    Ev: Fn(Event),
{
    // The output config the current sink was built for. Starts at the host's desired
    // config (the initial sink below), and tracks it: when the host mutates the shared
    // `output_desired` (a different device / exclusive mode), the pump loop rebuilds
    // the sink in place so the change applies to the current track — see below.
    let mut output_applied = output_desired.lock().expect("output config mutex").clone();
    // `output_live` is false when the deck is on the SILENT park placeholder because a sink
    // build FAILED (e.g. strict exclusive couldn't open the device, or there's no audio
    // device yet). The pump must STAY ALIVE in that case — a dead pump can't observe a
    // later config change (the user toggling exclusive/strict off) or a transient
    // device-in-use clearing, so a strict-exclusive failure would lock the user out until a
    // restart. Instead we park on silence and recover in the live-output block below.
    let (sink, mut output_live) = match make_sink(&output_applied) {
        Ok(s) => (s, true),
        Err(e) => {
            tracing::warn!(
                error = %format!("{e:#}"),
                "initial output sink unavailable (e.g. strict exclusive or no device); starting SILENT — will recover on a config change or when the device frees"
            );
            (make_park(), false)
        }
    };
    let rate = sink.stream_info().sample_rate;
    let deck = Deck::new(sink, Quality::Hq);
    let engine = Engine::new(deck, queue_for(Vec::new()));
    let mut player: ServicePlayer<K, OwnedDecoder> =
        Player::new(engine, u64::from(rate).saturating_mul(3)); // ~3 s prev threshold

    let (req_tx, req_rx) = mpsc::channel::<u32>();
    let (res_tx, res_rx) = mpsc::channel::<(u32, RgInfo, anyhow::Result<OwnedDecoder>)>();
    let loader = {
        let lib = Arc::clone(&library);
        // Same 64 MiB rationale as the pump: `open_track` builds an
        // `OwnedDecoder` (the big `AnyDecoder`) here, off the pump thread.
        thread::Builder::new()
            .name("player-loader".into())
            .stack_size(64 * 1024 * 1024)
            .spawn(move || loader_thread(req_rx, res_tx, lib))
            .expect("spawn loader thread")
    };

    // --- Hybrid ReplayGain: background loudness analysis of untagged tracks. The
    //     worker posts each completed `(path, gain)` back on `loud_res_rx` (like
    //     the loader's decoder results) so this pump — the one event source —
    //     emits the `TrackLoudnessAnalyzed` event. The cache is read synchronously
    //     at the RG gate below.
    let (loud_res_tx, loud_res_rx) = mpsc::channel::<(PathBuf, crate::TrackLoudness)>();
    let (loudness, analyzer) = LoudnessCache::spawn(loud_res_tx);

    let mut vol = VolumeState::new();
    let mut seed: u64 = SEED_STEP;
    let mut last_tick = Instant::now();
    let tick_every = Duration::from_millis(125); // ~8 Hz
    let mut disconnected = false;

    // --- Native-rate switching state ---
    // The native rate a reopen was last ATTEMPTED at; set only on a FAILED reopen so
    // we don't retry the same unsupported rate every pump (which would thrash the
    // device or loop). A different track rate, or a success, clears the suppression.
    let mut native_rate_tried: Option<u32> = None;

    // --- Live output-switching state ---
    // A desired output config whose device/sink last FAILED to open. We skip
    // re-applying the same failing config every block (which would park + rebuild the
    // device in a thrash loop). Cleared on a successful apply or when `desired` changes
    // to anything else.
    let mut output_apply_failed: Option<OutputConfig> = None;
    // When parked on the SILENT placeholder (`output_live == false`), re-attempt the
    // desired output on this cadence so a TRANSIENT failure (the device momentarily held
    // by another app) recovers on its own — Roon's "give the device a few seconds to
    // disengage from the last proprietor". A persistent failure keeps retrying cheaply;
    // a user toggling exclusive/strict off is a config change that bypasses the timer, so
    // recovery is instant.
    const OUTPUT_RETRY_EVERY: Duration = Duration::from_millis(1000);
    let mut last_output_retry = Instant::now();

    // --- ReplayGain state ---
    // Config, updated by `Command::SetReplayGain` (the host sends the persisted
    // choice right after spawn, so a session starts already normalized).
    let mut rg_mode = ReplayGainMode::Off;
    let mut rg_preamp: f32 = 0.0;
    // Per-track RG tags the loader read, banked by id as decoders arrive so the
    // right gain is ready when the track becomes current. Lives for the session;
    // the id space is one library's worth of tracks, so it stays small.
    let mut rg_tags: BTreeMap<u32, RgInfo> = BTreeMap::new();
    // The track the deck's current gain was computed for, and a dirty flag set
    // when the config changes — together they decide when to (re)apply the gain.
    // `None` + dirty means "apply once up front" (sets the deck to unity).
    let mut rg_applied_for: Option<u32> = None;
    let mut rg_dirty = true;

    loop {
        // 1. Drain commands.
        let mut changed = false;
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    match &cmd {
                        // Clamp/track volume at intake so `Tick.volume` matches what
                        // the deck applies, and a positive volume clears mute.
                        Command::SetVolume(v) => vol.set_volume(*v),
                        // Mute is modelled here — the core player has no mute — and
                        // resolves to a 0 effective gain below.
                        Command::ToggleMute => vol.toggle_mute(),
                        // Track the RG config here (the deck call happens below,
                        // keyed to the current track); `rg_dirty` forces a re-apply
                        // even when the track id hasn't changed.
                        Command::SetReplayGain { mode, preamp_db } => {
                            rg_mode = *mode;
                            rg_preamp = *preamp_db;
                            rg_dirty = true;
                        }
                        // Seed the in-memory loudness cache (pump state, lives here),
                        // not a transport call — a no-op in `apply_command`.
                        Command::SeedLoudness(entries) => loudness.seed(entries),
                        // Background loudness sweep: enqueue each track (single-flight skips
                        // already-analyzed / in-flight). Pump state, no-op in `apply_command`.
                        Command::AnalyzeTracks(entries) => {
                            for (id, path) in entries {
                                loudness.enqueue(*id, path.clone());
                            }
                        }
                        _ => {}
                    }
                    seed = seed.wrapping_add(SEED_STEP);
                    // Volume and mute both resolve to the effective gain on the deck
                    // (`VolumeState::gain`); every other command applies unchanged.
                    let applied = if matches!(cmd, Command::SetVolume(_) | Command::ToggleMute) {
                        Command::SetVolume(vol.gain())
                    } else {
                        cmd
                    };
                    if let Err(e) = apply_command(&mut player, applied, seed) {
                        tracing::warn!(error = ?e, "command error");
                    }
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        // 2. Install opened decoders; request whatever the engine now wants. Bank
        //    each track's RG tags (even on a failed open — harmless; it just won't
        //    become current) so the gain is ready the moment the track plays.
        while let Ok((id, rg, opened)) = res_rx.try_recv() {
            rg_tags.insert(id, rg);
            match opened {
                Ok(dec) => {
                    if let Ok(Provided::Loaded) = player.provide(id, dec) {
                        changed = true;
                    }
                }
                Err(e) => tracing::warn!(id, error = %format!("{e:#}"), "open track failed"),
            }
        }
        while let Some(id) = player.poll_open() {
            let _ = req_tx.send(id);
        }

        // 2b. Native-rate switching (bit-perfect exclusive output): before the pump
        //     writes this track's audio, reopen the exclusive device at the track's
        //     native rate when it differs — see `maybe_switch_native_rate` for the
        //     guards (shared sinks never reopen; the latch suppresses retrying a rate
        //     the device just refused; the current exclusive sink is kept on failure).
        maybe_switch_native_rate(&mut player, &mut native_rate_tried);

        // 2c. Live output switching: if the host changed the output device or
        //     exclusive mode since the last pump, apply it to the CURRENT track now
        //     (not just the next session; best practice — foobar/Roon apply
        //     immediately). `maybe_apply_output_change` does a release-then-acquire
        //     swap (park → drop old sink → build new on the freed device → swap),
        //     preserving position + ReplayGain, so a same-device exclusive switch
        //     doesn't hit device-in-use. A strict-only change is just recorded. On a
        //     failure it restores the old device (playback never dies) and we LATCH
        //     the failing config so we don't park/rebuild it every block.
        {
            let desired = output_desired.lock().expect("output config mutex").clone();
            if output_live {
                // Normal path: the deck is on a real device sink. Apply a device/exclusive
                // change live (release-then-acquire); skip re-attempting a config that just
                // failed (latched) to avoid per-block device thrash.
                if output_apply_failed.as_ref() != Some(&desired) {
                    match maybe_apply_output_change(
                        &mut player,
                        &desired,
                        &mut output_applied,
                        &make_sink,
                        &make_park,
                    ) {
                        OutputSwap::Applied => output_apply_failed = None,
                        OutputSwap::KeptCurrent(e) => {
                            // The change failed but a working sink still plays (the previous
                            // device, or a kept non-strict shared stream). Latch it.
                            output_apply_failed = Some(desired.clone());
                            tracing::warn!(?desired, error = %format!("{e:#}"), "live output change failed; kept the current device");
                        }
                        OutputSwap::Parked(e) => {
                            // The deck is SILENT now (a strict stop, or even the restore
                            // could not open). Drop to the parked state so the pump holds the
                            // position and recovers on a config change or the retry timer,
                            // instead of racing the queue through the silent placeholder.
                            output_apply_failed = Some(desired.clone());
                            output_live = false;
                            output_applied = desired.clone();
                            last_output_retry = Instant::now();
                            tracing::warn!(?desired, error = %format!("{e:#}"), "output could not open; stopped (silent); recovers on a config change or when the device frees");
                        }
                    }
                }
            } else {
                // Parked on the SILENT placeholder (a build failed). Recover when the config
                // changes (user toggles exclusive/strict off, or picks another device) OR on
                // the retry timer (a transient device-in-use clears). A config change
                // bypasses the timer so a user toggle recovers instantly.
                let config_changed = desired != output_applied;
                if config_changed || last_output_retry.elapsed() >= OUTPUT_RETRY_EVERY {
                    last_output_retry = Instant::now();
                    match rebuild_output_sink(
                        &mut player,
                        &desired,
                        &mut output_applied,
                        &make_sink,
                    ) {
                        Ok(()) => {
                            output_live = true;
                            output_apply_failed = None;
                            tracing::info!(
                                ?desired,
                                "output recovered; playback resumes on the device"
                            );
                        }
                        Err(e) => {
                            // Still unavailable — stay silent and retry on the next tick /
                            // config change. Debug level so a persistent failure doesn't
                            // spam the log every second.
                            output_apply_failed = Some(desired.clone());
                            tracing::debug!(?desired, error = %format!("{e:#}"), "output still unavailable; staying silent");
                        }
                    }
                }
            }
        }

        // 3. Pump one block (paced by the real sink's backpressure; NullSink
        //    returns immediately, so tests run briefly then drop the actor). While
        //    PARKED on the silent placeholder (a failed / strict-stopped output), do NOT
        //    pump: the placeholder has no backpressure, so pumping would race the track
        //    through the void at non-real-time speed and silently burn the queue. Hold the
        //    position until the device recovers; the loop paces itself via the Idle sleep.
        let progress = if output_live {
            match player.pump() {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(error = ?e, "pump error");
                    Progress::Idle
                }
            }
        } else {
            Progress::Idle
        };
        if matches!(progress, Progress::TrackChanged | Progress::Ended) {
            changed = true;
        }
        // On a track change, log the resolved signal path (decode/device rate, resampling,
        // exclusive) once — event-driven, so the pipeline is visible in the logs without
        // any per-block noise.
        if matches!(progress, Progress::TrackChanged)
            && let Some(sp) = project_signal_path(&player)
        {
            tracing::info!(
                decoded_rate = sp.decoded_rate,
                device_rate = sp.device_rate,
                resampling = sp.resampling,
                exclusive = sp.exclusive,
                "track signal path"
            );
        }

        // 3b. Drain finished background analyses (the hybrid-ReplayGain worker),
        //     emitting one event per newly-computed gain so the app can persist
        //     it. The worker already wrote the cache, so the gain is usable below
        //     and on the NEXT play of that track.
        while let Ok((path, loudness)) = loud_res_rx.try_recv() {
            on_event(Event::TrackLoudnessAnalyzed { path, loudness });
        }

        // 3c. Re-apply ReplayGain when the current track changes, or when the
        //     config changed (`rg_dirty`). The deck holds one constant gain, so a
        //     gapless advance sees the new track's gain updated ~one pump cycle
        //     after the swap (the id change is observed here, post-`pump`) — a
        //     known v1 limitation; exact per-decoder-slot gain across a gapless
        //     splice is a follow-up (it would require the deck to carry a gain
        //     per preload slot). A click isn't a concern: `set_replay_gain`
        //     applies a single factor with no ramp.
        //
        //     Hybrid (untagged) path: an untagged track under Track/Album folds in
        //     a *computed* loudness gain when the analyzer has one cached; a miss
        //     enqueues a single-flight analysis and plays preamp-only this time.
        //     A completed analysis is applied on the NEXT play of that track (when
        //     `cur` next changes to it), NOT mid-track — deliberately, to avoid an
        //     audible level jump partway through a song. (`resolve_rg_info` carries
        //     the full rationale.)
        let cur = player.current();
        if cur != rg_applied_for || rg_dirty {
            let info = resolve_rg_info(cur, rg_mode, &rg_tags, &library, &loudness);
            apply_replay_gain(&mut player, rg_mode, rg_preamp, info.as_ref());
            rg_applied_for = cur;
            rg_dirty = false;
        }

        // 4. Emit throttled state (immediately on change, else ~8 Hz).
        if changed || last_tick.elapsed() >= tick_every {
            let mut tick = project_tick(&player, vol.volume(), vol.muted());
            // The deck's duration comes from the decoder; some containers
            // (Matroska, CAF-ALAC) can't report a length, so fall back to the
            // library's tag-derived duration — keeping the progress bar
            // consistent with the track-list length.
            if tick.duration_secs.is_none()
                && let Some(id) = tick.now_playing_id
                && let Some(d) = library.lock().expect("library mutex").duration_of(id)
            {
                tick.duration_secs = Some(d);
            }
            // Parked on the SILENT placeholder (output stopped: strict exclusive could not
            // open, or the device vanished): the deck HOLDS its position but no audio
            // flows, so present it as PAUSED rather than a frozen "Playing" — the Settings
            // exclusive card carries the reason. Only an actually-playing deck is overridden
            // (a stopped/idle one stays as is), and the real state returns on recovery.
            if !output_live && tick.state == PlayState::Playing {
                tick.state = PlayState::Paused;
            }
            let cur_id = tick.now_playing_id;
            on_event(Event::Tick(tick));
            on_event(Event::Queue(project_queue(&player)));
            // Attach the current track's cached loudness metrics to the signal path
            // (the pure projection can't reach the actor's cache). Re-emitted every
            // tick, so it fills in within ~125 ms of the background analysis landing.
            let mut sp = project_signal_path(&player);
            if let Some(sp) = sp.as_mut()
                && let Some(id) = cur_id
                && let Some(path) = library.lock().expect("library mutex").path_of(id)
            {
                sp.loudness = loudness.metrics(&path);
            }
            on_event(Event::SignalPath(sp));
            last_tick = Instant::now();
        }

        // 5. Shutdown / idle pacing. The command channel disconnects only when
        //    the AudioPlayer handle is dropped — that IS the shutdown signal.
        //    Exit immediately rather than waiting for `!is_active()`: an active
        //    track would otherwise extend the join in `Drop` by the whole
        //    remaining playback (real-time-paced under CpalSink), and Repeat
        //    modes never become inactive, so the join would block forever.
        if disconnected {
            break;
        }
        if matches!(progress, Progress::Idle) {
            thread::sleep(Duration::from_millis(3));
        }
    }

    drop(req_tx); // stop the loader
    let _ = loader.join();
    // Close the job channel so the loudness worker exits, then join it. The join
    // can wait out one in-flight track's analysis (the same bounded shutdown
    // latency the loader has with an in-flight `open_track`); no analysis is ever
    // in flight under RG `Off`, so the common case joins instantly.
    drop(loudness);
    let _ = analyzer.join();
}

/// The [`RgInfo`] to apply for the current track under the hybrid policy: its read
/// RG tags, except that an *untagged* track (no track/album gain) under Track or
/// Album substitutes a background-computed loudness gain when the analyzer has one
/// cached — folded in as the **track** gain (which Album mode also falls back to,
/// matching the "no album gain computed → use the per-track gain" untagged policy).
/// On a cache miss it enqueues a single-flight analysis and returns the untagged
/// tags unchanged, so this play is preamp-only and the computed gain takes effect
/// only on the track's NEXT play. `Off` and already-tagged tracks pass straight
/// through — never analyzed.
fn resolve_rg_info(
    cur: Option<u32>,
    mode: ReplayGainMode,
    rg_tags: &BTreeMap<u32, RgInfo>,
    library: &Arc<Mutex<Arc<dyn TrackSource>>>,
    loudness: &LoudnessCache,
) -> Option<RgInfo> {
    let id = cur?;
    let tags = rg_tags.get(&id).copied();
    // Off never analyzes (and plays at unity regardless), so short-circuit.
    if mode == ReplayGainMode::Off {
        return tags;
    }
    let tagged = tags
        .map(|i| i.track_gain_db.is_some() || i.album_gain_db.is_some())
        .unwrap_or(false);
    if tagged {
        return tags;
    }
    // Untagged under Track/Album: resolve the path and consult the loudness cache.
    let path = library.lock().expect("library mutex").path_of(id)?;
    if let Some(gain) = loudness.lookup(&path) {
        Some(RgInfo {
            track_gain_db: Some(gain),
            album_gain_db: None,
        })
    } else {
        // Not analyzed yet — kick off a single-flight job (no-op if already
        // queued / analyzed) and play preamp-only this time.
        loudness.enqueue(id, path);
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;
    use crate::testutil::NullSink;
    use std::path::PathBuf;
    use std::sync::mpsc::RecvTimeoutError;

    fn wav(rate: u32, frames: u32) -> Vec<u8> {
        // Minimal stereo-16-bit WAV writer (same as library.rs tests).
        let channels: u16 = 2;
        let block_align = channels * 2;
        let byte_rate = rate * u32::from(block_align);
        let data_len = frames * u32::from(block_align);
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        v
    }

    fn fixture_library(dir: &PathBuf) -> Arc<Library> {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("a.wav"), wav(48_000, 96_000)).unwrap(); // 2 s
        std::fs::write(dir.join("b.wav"), wav(48_000, 96_000)).unwrap();
        Arc::new(Library::scan(dir).unwrap())
    }

    #[test]
    fn actor_plays_context_and_emits_now_playing() {
        let dir = std::env::temp_dir().join(format!("sas_actor_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = fixture_library(&dir);

        let (ev_tx, ev_rx) = mpsc::channel::<Event>();
        let player = AudioPlayer::spawn(
            |_cfg| Ok(NullSink::new(48_000, 2)),
            || NullSink::new(48_000, 2),
            Arc::new(Mutex::new(OutputConfig::default())),
            {
                let c = Arc::clone(&lib);
                let d: Arc<dyn TrackSource> = c;
                d
            },
            move |ev| {
                let _ = ev_tx.send(ev);
            },
        );

        player.send(Command::PlayContext {
            ids: vec![0, 1],
            start: 0,
        });

        // Wait (bounded) for a Tick showing track 0 playing.
        let deadline = Duration::from_secs(5);
        let mut saw_playing = false;
        loop {
            match ev_rx.recv_timeout(deadline) {
                Ok(Event::Tick(t)) => {
                    if t.now_playing_id == Some(0) && t.state == crate::types::PlayState::Playing {
                        saw_playing = true;
                        break;
                    }
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_playing, "never observed track 0 playing");

        drop(player); // clean shutdown (joins threads)
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The output-config WIRING: the pump watches the shared `output_desired` and,
    /// when the host changes the device (or exclusive mode) mid-playback, rebuilds
    /// the sink by calling `make_sink` again with the NEW config — so the change is
    /// live, not deferred to the next session. We record the device of every config
    /// `make_sink` builds for; flipping the device while track 0 plays must produce a
    /// second build carrying the new device name. (The pure apply mechanism —
    /// position / ReplayGain preservation — is covered by `driver::tests`; this
    /// proves the pump is wired to it.)
    #[test]
    fn changing_output_config_mid_play_rebuilds_the_sink_live() {
        let dir = std::env::temp_dir().join(format!("sas_actor_outcfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = fixture_library(&dir);

        // Record the device of every config the pump builds a sink for.
        let built: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let desired = Arc::new(Mutex::new(OutputConfig::default()));

        let (ev_tx, ev_rx) = mpsc::channel::<Event>();
        let player = AudioPlayer::spawn(
            {
                let built = Arc::clone(&built);
                move |cfg: &OutputConfig| {
                    built.lock().unwrap().push(cfg.device.clone());
                    Ok(NullSink::new(48_000, 2))
                }
            },
            || NullSink::new(48_000, 2),
            Arc::clone(&desired),
            {
                let c = Arc::clone(&lib);
                let d: Arc<dyn TrackSource> = c;
                d
            },
            move |ev| {
                let _ = ev_tx.send(ev);
            },
        );

        player.send(Command::PlayContext {
            ids: vec![0, 1],
            start: 0,
        });
        assert!(
            wait_for_now_playing(&ev_rx, 0, 2.0),
            "track 0 never started"
        );

        // The initial sink was built once, for the default (system-default) device.
        assert_eq!(
            built.lock().unwrap().as_slice(),
            &[None],
            "exactly one build (the initial default-device sink) before any change"
        );

        // Host switches the output device live.
        desired.lock().unwrap().device = Some("Living Room DAC".to_string());

        // The pump must rebuild the sink for the NEW device within a bounded window.
        let mut rebuilt = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if built
                .lock()
                .unwrap()
                .iter()
                .any(|d| d.as_deref() == Some("Living Room DAC"))
            {
                rebuilt = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            rebuilt,
            "device change was not applied live; builds = {:?}",
            built.lock().unwrap()
        );

        drop(player);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The STRICT-blocking regression: when the output sink can't be built at startup
    /// (e.g. strict exclusive-or-stop and the device is in use), the pump must NOT die —
    /// a dead pump can't observe the user toggling exclusive/strict off or the device
    /// freeing, so it would lock the user out until a restart. It must instead start
    /// SILENT, keep emitting ticks (alive), retry the output, and RECOVER once the build
    /// can succeed. We fail the build while a flag is set, then clear it.
    #[test]
    fn a_failed_output_sink_keeps_the_pump_alive_and_recovers() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let dir = std::env::temp_dir().join(format!("sas_actor_strict_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = fixture_library(&dir);

        let fail = Arc::new(AtomicBool::new(true));
        let builds = Arc::new(AtomicUsize::new(0));
        let successes = Arc::new(AtomicUsize::new(0));
        let make_sink = {
            let fail = Arc::clone(&fail);
            let builds = Arc::clone(&builds);
            let successes = Arc::clone(&successes);
            move |_cfg: &OutputConfig| -> anyhow::Result<NullSink> {
                builds.fetch_add(1, Ordering::Relaxed);
                if fail.load(Ordering::Relaxed) {
                    anyhow::bail!("exclusive output unavailable (strict, test)");
                }
                successes.fetch_add(1, Ordering::Relaxed);
                Ok(NullSink::new(48_000, 2))
            }
        };

        let (ev_tx, ev_rx) = mpsc::channel::<Event>();
        let player = AudioPlayer::spawn(
            make_sink,
            || NullSink::new(48_000, 2), // silent park placeholder (same K)
            Arc::new(Mutex::new(OutputConfig {
                device: None,
                exclusive: true,
                strict: true,
            })),
            {
                let c = Arc::clone(&lib);
                let d: Arc<dyn TrackSource> = c;
                d
            },
            move |ev| {
                let _ = ev_tx.send(ev);
            },
        );

        // The initial strict build FAILS — the old code would `return` here and the pump
        // thread would be gone. Prove it is alive: it keeps emitting ticks AND retries the
        // output build (> the one initial attempt), without ever opening a sink.
        player.send(Command::PlayContext {
            ids: vec![0, 1],
            start: 0,
        });
        let mut got_tick = false;
        // While parked (silent), a loaded track must present as PAUSED, never a frozen
        // "Playing" (the strict-stop UX fix) — the deck holds its position but no audio
        // flows.
        let mut saw_playing_while_parked = false;
        let mut saw_paused_track = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Event::Tick(t)) = ev_rx.recv_timeout(Duration::from_millis(200)) {
                got_tick = true;
                if t.state == PlayState::Playing {
                    saw_playing_while_parked = true;
                }
                if t.now_playing_id == Some(0) && t.state == PlayState::Paused {
                    saw_paused_track = true;
                }
            }
            if got_tick && builds.load(Ordering::Relaxed) >= 2 && saw_paused_track {
                break;
            }
        }
        assert!(
            got_tick,
            "a dead pump emits no ticks — the pump must stay alive while the output is parked"
        );
        assert!(
            !saw_playing_while_parked,
            "a parked (silent) output must present as paused, never a frozen Playing"
        );
        assert!(
            saw_paused_track,
            "the loaded track reads as paused while the output is parked"
        );
        assert!(
            builds.load(Ordering::Relaxed) >= 2,
            "the parked pump must RETRY the output (proving it's alive), builds={}",
            builds.load(Ordering::Relaxed)
        );
        assert_eq!(
            successes.load(Ordering::Relaxed),
            0,
            "no sink is opened while the device is unavailable (strict: never the OS mixer)"
        );

        // The device frees up → the next retry must RECOVER (a real sink opens).
        fail.store(false, Ordering::Relaxed);
        let mut recovered = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if successes.load(Ordering::Relaxed) > 0 {
                recovered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            recovered,
            "the output must recover (a sink opens) once the device frees — the user is \
             never locked out"
        );

        drop(player);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Wait (bounded) for a Tick showing `id` playing with duration ≈ `approx` s.
    /// Ticks for any other track/duration are skipped; returns false on timeout.
    fn wait_for_now_playing(rx: &mpsc::Receiver<Event>, id: u32, approx: f64) -> bool {
        let deadline = Duration::from_secs(5);
        loop {
            match rx.recv_timeout(deadline) {
                Ok(Event::Tick(t)) => {
                    if t.now_playing_id == Some(id)
                        && t.state == crate::types::PlayState::Playing
                        && t.duration_secs.is_some_and(|d| (d - approx).abs() < 0.25)
                    {
                        return true;
                    }
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    /// Guards the folder-switch / loader race the retired Tauri e2e test
    /// (`folder_switch_loads_the_new_library`, #2) covered flakily: after the library
    /// is swapped mid-playback, a play against the NEW library's ids must resolve and
    /// play the NEW track. The loader reads the shared library fresh on every request
    /// (there is no stale-library window), so this is deterministic — proven by the
    /// durations: A's tracks are 2 s, B's are 1 s, so the now-playing duration pins
    /// which library resolved the id.
    #[test]
    fn swapping_the_library_mid_play_resolves_the_new_library() {
        let base = std::env::temp_dir().join(format!("sas_actor_swap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (dir_a, dir_b) = (base.join("A"), base.join("B"));
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        std::fs::write(dir_a.join("a.wav"), wav(48_000, 96_000)).unwrap(); // 2.0 s
        std::fs::write(dir_a.join("a2.wav"), wav(48_000, 96_000)).unwrap();
        std::fs::write(dir_b.join("b.wav"), wav(48_000, 48_000)).unwrap(); // 1.0 s
        let lib_a = Arc::new(Library::scan(&dir_a).unwrap());
        let lib_b = Arc::new(Library::scan(&dir_b).unwrap());

        let (ev_tx, ev_rx) = mpsc::channel::<Event>();
        let player = AudioPlayer::spawn(
            |_cfg| Ok(NullSink::new(48_000, 2)),
            || NullSink::new(48_000, 2),
            Arc::new(Mutex::new(OutputConfig::default())),
            {
                let c = Arc::clone(&lib_a);
                let d: Arc<dyn TrackSource> = c;
                d
            },
            move |ev| {
                let _ = ev_tx.send(ev);
            },
        );

        // Play library A; confirm A's 2 s track 0 is the now-playing track.
        player.send(Command::PlayContext {
            ids: vec![0, 1],
            start: 0,
        });
        assert!(
            wait_for_now_playing(&ev_rx, 0, 2.0),
            "A's 2 s track 0 never became now-playing"
        );

        // Swap the loader's library to B mid-play, then play B's track 0. The
        // now-playing track must resolve to B (≈1 s) — not stale A (2 s).
        player.set_library(lib_b);
        player.send(Command::PlayContext {
            ids: vec![0],
            start: 0,
        });
        assert!(
            wait_for_now_playing(&ev_rx, 0, 1.0),
            "after the swap, B's 1 s track never became now-playing — the loader \
             resolved the id against the stale library (the #2 folder-switch race)"
        );

        drop(player);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn drop_returns_promptly_even_while_repeat_all_plays() {
        let dir = std::env::temp_dir().join(format!("sas_actor_drop_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = fixture_library(&dir);

        let (ev_tx, ev_rx) = mpsc::channel::<Event>();
        let player = AudioPlayer::spawn(
            |_cfg| Ok(NullSink::new(48_000, 2)),
            || NullSink::new(48_000, 2),
            Arc::new(Mutex::new(OutputConfig::default())),
            {
                let c = Arc::clone(&lib);
                let d: Arc<dyn TrackSource> = c;
                d
            },
            move |ev| {
                let _ = ev_tx.send(ev);
            },
        );

        // Repeat::All means the player never becomes inactive on its own;
        // dropping the handle must still shut the pump down promptly.
        // (Regression: the old shutdown condition also required `!is_active()`
        // so `Drop`'s join blocked forever while repeat playback was active.)
        player.send(Command::SetRepeat(crate::types::RepeatMode::All));
        player.send(Command::PlayContext {
            ids: vec![0, 1],
            start: 0,
        });

        // Wait until a Tick shows track 0 playing (loader round-trip done).
        let mut saw_playing = false;
        let deadline = Duration::from_secs(5);
        loop {
            match ev_rx.recv_timeout(deadline) {
                Ok(Event::Tick(t)) => {
                    if t.now_playing_id == Some(0) && t.state == crate::types::PlayState::Playing {
                        saw_playing = true;
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(saw_playing, "never observed track 0 playing");

        // Drop on a helper thread: the join must return promptly, not block
        // on Repeat::All playback that would otherwise never end.
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let dropper = std::thread::spawn(move || {
            drop(player);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "drop(player) blocked: pump thread did not exit while repeat playback was active"
        );
        let _ = dropper.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- hybrid ReplayGain: untagged-track gain resolution ------------------

    #[test]
    fn resolve_rg_info_folds_a_cached_gain_for_an_untagged_track() {
        let dir = std::env::temp_dir().join(format!("sas_rg_fold_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = fixture_library(&dir);
        let ts: Arc<dyn TrackSource> = lib;
        let library = Arc::new(Mutex::new(ts));

        // A cache seeded (as `SeedLoudness` would) with a computed gain for the
        // untagged WAV at id 0 — no worker interaction needed to observe the fold.
        let (res_tx, _res_rx) = mpsc::channel::<(PathBuf, crate::TrackLoudness)>();
        let (loudness, _worker) = LoudnessCache::spawn(res_tx);
        let path0 = library.lock().unwrap().path_of(0).unwrap();
        // integrated −10.5 LUFS → derived RG gain −7.5 dB (to the −18 target).
        loudness.seed(&[(
            path0,
            crate::TrackLoudness {
                integrated_lufs: -10.5,
                lra: 0.0,
                true_peak_dbtp: 0.0,
                plr: 0.0,
                dr: 0.0,
            },
        )]);
        let rg_tags: BTreeMap<u32, RgInfo> = BTreeMap::new(); // WAV fixtures: untagged

        // Track mode: the cached gain stands in as the track gain.
        let info = resolve_rg_info(
            Some(0),
            ReplayGainMode::Track,
            &rg_tags,
            &library,
            &loudness,
        );
        assert_eq!(
            info,
            Some(RgInfo {
                track_gain_db: Some(-7.5),
                album_gain_db: None,
            }),
            "a cached gain folds in as the track gain"
        );
        assert!(
            (crate::driver::applied_gain_db(ReplayGainMode::Track, 0.0, info.as_ref()) - (-7.5))
                .abs()
                < 1e-6
        );
        // Album mode on an untagged track falls back to the same per-track gain.
        let album = resolve_rg_info(
            Some(0),
            ReplayGainMode::Album,
            &rg_tags,
            &library,
            &loudness,
        );
        assert!(
            (crate::driver::applied_gain_db(ReplayGainMode::Album, 0.0, album.as_ref()) - (-7.5))
                .abs()
                < 1e-6,
            "Album reuses the computed per-track gain when no album gain exists"
        );
        // Off never folds (and never analyzes).
        assert_eq!(
            resolve_rg_info(Some(0), ReplayGainMode::Off, &rg_tags, &library, &loudness),
            None,
            "Off passes through untouched"
        );

        drop(loudness);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_rg_info_plays_preamp_only_on_a_cache_miss() {
        let dir = std::env::temp_dir().join(format!("sas_rg_miss_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = fixture_library(&dir);
        let ts: Arc<dyn TrackSource> = lib;
        let library = Arc::new(Mutex::new(ts));

        let (res_tx, _res_rx) = mpsc::channel::<(PathBuf, crate::TrackLoudness)>();
        let (loudness, _worker) = LoudnessCache::spawn(res_tx);
        let rg_tags: BTreeMap<u32, RgInfo> = BTreeMap::new();

        // Nothing cached for id 0 → the untagged tags pass through (preamp-only),
        // and a background analysis is enqueued as a side effect (single-flight,
        // exercised directly in `loudness`'s own tests).
        let info = resolve_rg_info(
            Some(0),
            ReplayGainMode::Track,
            &rg_tags,
            &library,
            &loudness,
        );
        assert_eq!(info, None, "a miss plays preamp-only this time");

        drop(loudness);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
