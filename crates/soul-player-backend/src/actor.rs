//! The player actor: two non-real-time threads around a `Player`.
//! - **pump**: owns the `Player` + sink, drains the command channel, services
//!   decoder opens, pumps one block per cycle, and emits throttled state.
//! - **loader**: resolves `id → path` against the current `Library` and opens
//!   the (large, `Send`) `OwnedDecoder` off the pump thread.
//!
//! Sink-generic via a factory run ON the pump thread, so a `!Send` device sink
//! (`CpalSink`) works and this crate never names `cpal`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use soul_audio_control::Player;
use soul_audio_core::{Deck, Progress, Quality, Sink};
use soul_audio_desktop::{OwnedDecoder, open_track};
use soul_audio_engine::{Engine, Provided};

use crate::driver::{
    RgInfo, ServicePlayer, apply_command, apply_replay_gain, project_queue, project_tick, queue_for,
};
use crate::library::Library;
use crate::types::{Command, Event, ReplayGainMode};

/// Golden-ratio odd constant; advanced per shuffle to vary the permutation seed
/// without an RNG dependency (deterministic across runs).
const SEED_STEP: u64 = 0x9E37_79B9_7F4A_7C15;

/// Handle to the running player actor. Dropping it stops the threads.
pub struct AudioPlayer {
    cmd_tx: Option<Sender<Command>>,
    library: Arc<Mutex<Arc<Library>>>,
    pump: Option<JoinHandle<()>>,
}

impl AudioPlayer {
    /// Spawn the actor. `make_sink` runs on the pump thread (so `K` need not be
    /// `Send`). `on_event` is called from the pump thread on every emitted
    /// event. `library` is the initial `id → path` map (swap later via
    /// [`set_library`](Self::set_library)).
    pub fn spawn<K, Mk, Ev>(make_sink: Mk, library: Arc<Library>, on_event: Ev) -> Self
    where
        K: Sink + 'static,
        Mk: FnOnce() -> anyhow::Result<K> + Send + 'static,
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
            .spawn(move || pump_thread(make_sink, cmd_rx, loader_lib, on_event))
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
    pub fn set_library(&self, library: Arc<Library>) {
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

/// The loader thread: `id → path → (ReplayGain tags, OwnedDecoder)`, sent back to
/// the pump. The RG tags are read here, on the loader thread, so the pump's hot
/// loop never touches the filesystem for them; they ride the same message as the
/// decoder, keyed to the id, and the pump banks them for when the track becomes
/// current (see [`pump_thread`]).
fn loader_thread(
    req_rx: Receiver<u32>,
    res_tx: Sender<(u32, RgInfo, anyhow::Result<OwnedDecoder>)>,
    library: Arc<Mutex<Arc<Library>>>,
) {
    for id in req_rx {
        let (rg, opened) = {
            let lib = library.lock().expect("library mutex").clone();
            match lib.path_of(id) {
                Some(path) => (read_rg_tags(&path), open_track(&path)),
                None => (
                    RgInfo::default(),
                    Err(anyhow::anyhow!("no path for track id {id}")),
                ),
            }
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

fn pump_thread<K, Mk, Ev>(
    make_sink: Mk,
    cmd_rx: Receiver<Command>,
    library: Arc<Mutex<Arc<Library>>>,
    on_event: Ev,
) where
    K: Sink + 'static,
    Mk: FnOnce() -> anyhow::Result<K>,
    Ev: Fn(Event),
{
    let sink = match make_sink() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("player: sink init failed: {e:#}");
            return;
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

    let mut volume: f32 = 1.0;
    let mut seed: u64 = SEED_STEP;
    let mut last_tick = Instant::now();
    let tick_every = Duration::from_millis(125); // ~8 Hz
    let mut disconnected = false;

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
                    if let Command::SetVolume(v) = cmd {
                        // Clamp at intake so projected `Tick.volume` matches what
                        // `Deck::set_volume` actually applies (0.0..=1.0).
                        volume = v.clamp(0.0, 1.0);
                    }
                    if let Command::SetReplayGain { mode, preamp_db } = cmd {
                        // Track the config here (the deck call happens below, keyed
                        // to the current track); `rg_dirty` forces a re-apply even
                        // when the track id hasn't changed.
                        rg_mode = mode;
                        rg_preamp = preamp_db;
                        rg_dirty = true;
                    }
                    seed = seed.wrapping_add(SEED_STEP);
                    if let Err(e) = apply_command(&mut player, cmd, seed) {
                        eprintln!("player: command error: {e:?}");
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
                Err(e) => eprintln!("player: open track {id} failed: {e:#}"),
            }
        }
        while let Some(id) = player.poll_open() {
            let _ = req_tx.send(id);
        }

        // 3. Pump one block (paced by the real sink's backpressure; NullSink
        //    returns immediately, so tests run briefly then drop the actor).
        let progress = match player.pump() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("player: pump error: {e:?}");
                Progress::Idle
            }
        };
        if matches!(progress, Progress::TrackChanged | Progress::Ended) {
            changed = true;
        }

        // 3b. Re-apply ReplayGain when the current track changes, or when the
        //     config changed (`rg_dirty`). The deck holds one constant gain, so a
        //     gapless advance sees the new track's gain updated ~one pump cycle
        //     after the swap (the id change is observed here, post-`pump`) — a
        //     known v1 limitation; exact per-decoder-slot gain across a gapless
        //     splice is a follow-up (it would require the deck to carry a gain
        //     per preload slot). A click isn't a concern: `set_replay_gain`
        //     applies a single factor with no ramp.
        //     TODO(untagged-analysis): tracks with no RG tags get only the preamp
        //     here; computing a gain for them by background-analyzing loudness is
        //     a deliberate follow-up — this is the hook where its result would be
        //     folded into `rg_tags` before the gain is computed.
        let cur = player.current();
        if cur != rg_applied_for || rg_dirty {
            apply_replay_gain(
                &mut player,
                rg_mode,
                rg_preamp,
                cur.and_then(|id| rg_tags.get(&id)),
            );
            rg_applied_for = cur;
            rg_dirty = false;
        }

        // 4. Emit throttled state (immediately on change, else ~8 Hz).
        if changed || last_tick.elapsed() >= tick_every {
            let mut tick = project_tick(&player, volume);
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
            on_event(Event::Tick(tick));
            on_event(Event::Queue(project_queue(&player)));
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
            || Ok(NullSink::new(48_000, 2)),
            Arc::clone(&lib),
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
                Ok(Event::Queue(_)) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw_playing, "never observed track 0 playing");

        drop(player); // clean shutdown (joins threads)
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn drop_returns_promptly_even_while_repeat_all_plays() {
        let dir = std::env::temp_dir().join(format!("sas_actor_drop_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = fixture_library(&dir);

        let (ev_tx, ev_rx) = mpsc::channel::<Event>();
        let player = AudioPlayer::spawn(
            || Ok(NullSink::new(48_000, 2)),
            Arc::clone(&lib),
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
}
