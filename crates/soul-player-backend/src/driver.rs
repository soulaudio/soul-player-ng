//! The concrete desktop `Player` specialization plus the pure functions that
//! translate an app [`Command`] into `Player` calls and project the `Player`'s
//! state into UI [`Tick`]/[`QueueState`] events. Generic over the sink and
//! decoder so the whole thing is unit-tested headlessly (NullSink + a const
//! decoder) exactly as it runs with a real device.

use std::collections::VecDeque;

use soul_audio_control::Player;
use soul_audio_core::{DeckError, SampleFormat, SeekableDecoder, Sink};
use soul_audio_queue::{Repeat, SliceContext, TwoLevelQueue};

use crate::types::{
    Command, PlayState, QueueState, ReplayGainMode, SignalPath, SourceFormat, Tick,
};

/// The desktop player: unbounded (`VecDeque`) Up-Next/history, `SliceContext`
/// over `u32` track ids, `PRELOAD = 2`. Generic over the sink `K` and decoder
/// `D` (production: `CpalSink`/`OwnedDecoder`; tests: `NullSink`/`ConstDecoder`).
pub type ServicePlayer<K, D> =
    Player<K, D, SliceContext<u32, Vec<u32>>, VecDeque<u32>, VecDeque<(u32, usize)>, 2>;

/// The concrete desktop queue: `SliceContext` over `u32` track ids, unbounded
/// (`VecDeque`) Up-Next/history. Factored out of `queue_for`'s signature per
/// `clippy::type_complexity`.
type ServiceQueue =
    TwoLevelQueue<SliceContext<u32, Vec<u32>>, VecDeque<u32>, VecDeque<(u32, usize)>>;

/// Build the concrete desktop queue over a set of track ids (empty at start).
pub fn queue_for(ids: Vec<u32>) -> ServiceQueue {
    TwoLevelQueue::new(SliceContext::new(ids), VecDeque::new(), VecDeque::new())
}

/// Apply one UI command to the player. `seed` is used only by `SetShuffle`
/// (the caller supplies a fresh value each time). Transport errors propagate;
/// bounded-queue-full is impossible here (unbounded backings) but `play_next`/
/// `enqueue` still swallow it for uniformity.
pub fn apply_command<K: Sink, D: SeekableDecoder>(
    player: &mut ServicePlayer<K, D>,
    cmd: Command,
    seed: u64,
) -> Result<(), DeckError> {
    match cmd {
        Command::PlayContext { ids, start } => {
            player.set_source(SliceContext::new(ids), start as usize);
            Ok(())
        }
        Command::TogglePlay => {
            player.toggle_play();
            Ok(())
        }
        Command::Play => {
            player.play();
            Ok(())
        }
        Command::Pause => {
            player.pause();
            Ok(())
        }
        Command::Stop => {
            player.stop();
            Ok(())
        }
        Command::Next => player.next(),
        Command::Prev => player.prev(),
        Command::Seek { secs } => {
            let frame = (secs.max(0.0) * f64::from(player.sample_rate())) as u64;
            player.seek_frames(frame).map(|_| ())
        }
        Command::SetVolume(v) => {
            player.set_volume(v);
            Ok(())
        }
        // Mute is resolved to an effective gain (`VolumeState::gain`) and applied
        // as `SetVolume` by the actor before this call; it carries no transport
        // action of its own, so here it is a no-op.
        Command::ToggleMute => Ok(()),
        Command::PlayNext(id) => {
            let _ = player.play_next(id);
            Ok(())
        }
        Command::Enqueue(id) => {
            let _ = player.enqueue(id);
            Ok(())
        }
        Command::ClearQueue => {
            player.clear_queue();
            Ok(())
        }
        Command::RemoveFromQueue(i) => {
            let _ = player.remove_from_queue(i as usize);
            Ok(())
        }
        Command::MoveInQueue { from, to } => {
            let _ = player.move_in_queue(from as usize, to as usize);
            Ok(())
        }
        Command::PlayUpcoming(k) => {
            let _ = player.play_upcoming(k as usize);
            Ok(())
        }
        Command::SetShuffle(on) => {
            player.set_shuffle(on, seed);
            Ok(())
        }
        Command::SetRepeat(mode) => {
            player.set_repeat(mode.into());
            Ok(())
        }
        Command::Goto(i) => player.goto(i as usize),
        // ReplayGain is applied by the pump directly against the deck (it needs
        // the per-track RG tags the loader read, which `apply_command` has no
        // access to) — there is no `Player` transport call for it, so here it is
        // a no-op. See [`apply_replay_gain`].
        Command::SetReplayGain { .. } => Ok(()),
        // Seeding the loudness cache is pump state (the cache lives there), not a
        // transport action — the pump intercepts it before this call. No-op here.
        Command::SeedLoudness(_) => Ok(()),
        // The background loudness sweep enqueues on the pump's cache; intercepted there.
        Command::AnalyzeTracks(_) => Ok(()),
        // The analysis mode is pump state (gates the sweep + the worker pause); the pump
        // records it before this call. No transport action.
        Command::SetLoudnessAnalysisMode(_) => Ok(()),
    }
}

/// Per-track ReplayGain tag data, read by the loader off the pump thread and
/// keyed to the track id so the pump can apply the matching gain when that track
/// becomes current. Both fields are `None` for an untagged track (then only the
/// preamp applies). Peaks are deliberately omitted: tag-based v1 gain never reads
/// them; a peak-limited / clipping-prevention mode is a follow-up that would add
/// them here.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RgInfo {
    /// ReplayGain/R128 track gain, in dB (as parsed, reference level un-reconciled).
    pub track_gain_db: Option<f32>,
    /// ReplayGain/R128 album gain, in dB.
    pub album_gain_db: Option<f32>,
}

/// The dB to apply for one track under `mode` + `preamp_db`, given its RG tags
/// (`None` = untagged). Pure — the single source of truth for the normalization
/// policy, unit-tested directly:
/// - `Off` → `0.0` (unity; the preamp is intentionally ignored so "off" is a
///   bit-identical passthrough).
/// - `Track` → track gain (or `0.0` if that tag is absent) `+ preamp`.
/// - `Album` → album gain, falling back to the track gain, (or `0.0`) `+ preamp`.
#[must_use]
pub fn applied_gain_db(mode: ReplayGainMode, preamp_db: f32, info: Option<&RgInfo>) -> f32 {
    match mode {
        ReplayGainMode::Off => 0.0,
        ReplayGainMode::Track => info.and_then(|i| i.track_gain_db).unwrap_or(0.0) + preamp_db,
        ReplayGainMode::Album => {
            info.and_then(|i| i.album_gain_db.or(i.track_gain_db))
                .unwrap_or(0.0)
                + preamp_db
        }
    }
}

/// Compute (via [`applied_gain_db`]) and apply the current track's ReplayGain to
/// the deck, returning the applied dB. The deck clamps and converts to a linear
/// factor; `0.0` dB is an exact passthrough. The pump calls this at each track
/// boundary and whenever the mode/preamp change; it is factored out so the
/// normalization effect is unit-testable device-free against a `NullSink` deck.
pub fn apply_replay_gain<K: Sink, D: SeekableDecoder>(
    player: &mut ServicePlayer<K, D>,
    mode: ReplayGainMode,
    preamp_db: f32,
    info: Option<&RgInfo>,
) -> f32 {
    let db = applied_gain_db(mode, preamp_db, info);
    player.deck_mut().set_replay_gain(db);
    db
}

fn frames_to_secs(frames: u64, rate: u32) -> f64 {
    if rate == 0 {
        0.0
    } else {
        frames as f64 / f64::from(rate)
    }
}

/// Project the now-playing/transport snapshot. `volume` is tracked by the caller
/// (the `Player` has no volume getter).
pub fn project_tick<K: Sink, D: SeekableDecoder>(
    player: &ServicePlayer<K, D>,
    volume: f32,
    muted: bool,
) -> Tick {
    let rate = player.sample_rate();
    Tick {
        now_playing_id: player.current(),
        position_secs: frames_to_secs(player.position_frames(), rate),
        duration_secs: player.duration_frames().map(|f| frames_to_secs(f, rate)),
        state: PlayState::from(player.state()),
        shuffle: player.is_shuffled(),
        repeat: player.repeat().into(),
        volume,
        muted,
    }
}

/// The session's volume + mute — the source of truth for the gain the deck runs
/// at, and for `Tick.{volume, muted}`. The core player has no mute, so muting is
/// modelled here and applied as a 0 effective [`gain`](VolumeState::gain). Kept
/// a small, pure value type so the mute rules are unit-tested without a deck.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumeState {
    volume: f32,
    muted: bool,
}

impl VolumeState {
    /// Full volume, un-muted — the session default.
    pub fn new() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }

    /// Set the user volume (clamped to 0.0..=1.0). A positive volume clears mute,
    /// so dragging the slider un-mutes (the old app's VolumeControl). Setting it
    /// to exactly 0 is *not* a mute — it leaves the mute flag untouched.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if self.volume > 0.0 {
            self.muted = false;
        }
    }

    /// Flip mute on/off, preserving the volume to restore.
    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
    }

    /// The user's volume level (what the slider shows), independent of mute.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Whether the output is currently muted.
    pub fn muted(&self) -> bool {
        self.muted
    }

    /// The gain actually applied to the deck: silent while muted, else `volume`.
    pub fn gain(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }
}

impl Default for VolumeState {
    fn default() -> Self {
        Self::new()
    }
}

/// How many upcoming CONTEXT tracks (the album/playlist remainder after the
/// manual queue) to project for the "Next from" band. Bounded so a huge library —
/// or `Repeat::All`'s infinite stream — can't grow the snapshot unboundedly; the
/// sidebar only ever shows a scrollable handful.
const UPCOMING_CONTEXT_CAP: usize = 64;

/// Project the Up-Next snapshot: the manual queue (`up_next_ids`) and the real
/// upcoming CONTEXT tracks (`context_ids`) the player will actually play after it,
/// honouring shuffle — so the UI renders FROM the player instead of guessing from
/// library order (which is wrong once shuffle is on).
pub fn project_queue<K: Sink, D: SeekableDecoder>(player: &ServicePlayer<K, D>) -> QueueState {
    let q = player.sequencer();
    let manual = q.upnext_len();
    // `upcoming_iter` is INFINITE under Repeat::All (it loops the context) and
    // Repeat::One (it freezes on the current). Taken naively that fills the
    // "Next from…" band with the same tracks over and over — the "shows tracks
    // twice" bug. Bound it to ONE pass over the other source tracks (`source_len-1`,
    // the whole context minus the current), still capped for a huge library; under
    // Repeat::One the source is frozen, so there is no upcoming context at all.
    let context_cap = if q.repeat() == Repeat::One {
        0
    } else {
        q.source_len().saturating_sub(1).min(UPCOMING_CONTEXT_CAP)
    };
    QueueState {
        up_next_ids: q.upnext_iter().collect(),
        context_ids: q.upcoming_iter().skip(manual).take(context_cap).collect(),
        context_pos: q.pos() as u32,
        context_len: q.source_len() as u32,
    }
}

/// Project the current track's **signal path** — source → decode → ReplayGain →
/// resample → output — straight from the deck's real decode chain, so the display
/// matches what the engine actually does (DSD reads as DSD; a rate match reads as
/// no-resample / sample-rate-exact). `None` when nothing is loaded.
#[must_use]
pub fn project_signal_path<K: Sink, D: SeekableDecoder>(
    player: &ServicePlayer<K, D>,
) -> Option<SignalPath> {
    let deck = player.deck();
    let di = deck.decode_info()?;
    let source = match di.source {
        SampleFormat::Dsd { rate } => SourceFormat::Dsd { rate_hz: rate.hz() },
        other => SourceFormat::Pcm {
            bits: other.bits_per_sample(),
        },
    };
    Some(SignalPath {
        source,
        codec: di.codec.into(),
        decoded_rate: di.decoded.sample_rate,
        decoded_bits: di.decoded.format.bits_per_sample(),
        channels: di.decoded.channels as u8,
        replay_gain_db: deck.replay_gain_db(),
        device_rate: di.device_rate,
        resampling: di.resampling,
        // The device's real output depth (shared mixer format, or the negotiated
        // bit-perfect depth in exclusive mode), sourced from the sink — not a
        // hardcoded canonical 24.
        output_bits: di.output_format.bits_per_sample(),
        exclusive: di.exclusive,
        // The actor fills this from its loudness cache (this projection is pure).
        loudness: None,
    })
}

/// The native rate to reopen the output device at for bit-perfect exclusive output,
/// or `None` to leave the sink as is. Reopen only when the sink is **exclusive**, the
/// track's `decoded_rate` differs from the current `device_rate`, and we didn't just
/// fail at that rate (`tried`). Pure so the loop-safety guard is unit-tested without a
/// device: a shared sink never reopens; a rate the device just refused isn't retried
/// (the caller sets `tried` on failure); a new/different rate is tried again.
#[must_use]
pub fn native_rate_target(
    exclusive: bool,
    decoded_rate: u32,
    device_rate: u32,
    tried: Option<u32>,
) -> Option<u32> {
    if exclusive && decoded_rate != device_rate && tried != Some(decoded_rate) {
        Some(decoded_rate)
    } else {
        None
    }
}

/// Native-rate switching (bit-perfect exclusive output): if the sink is driving the
/// device **exclusively** and the current track's native rate differs from the device
/// rate, reopen the device **in place** at the track's rate ([`Deck::reopen_sink_at`])
/// so the track plays with no resampling. In-place is essential: WASAPI exclusive is
/// one-client-per-device, so opening a *second* exclusive stream to swap in would fail
/// with device-in-use — the one sink must release + re-acquire.
///
/// `tried` is the caller's anti-thrash latch (threaded through the pump loop): it is
/// latched whenever the reopen does NOT land on `want` — an error, or the sink falling
/// back to its old rate because the device refused the new one — so the same rate isn't
/// retried every pump (which would thrash the device). Only landing on `want` clears it.
/// A shared sink (`exclusive() == false`) never reopens.
pub fn maybe_switch_native_rate<K: Sink, D: SeekableDecoder>(
    player: &mut ServicePlayer<K, D>,
    tried: &mut Option<u32>,
) {
    if let Some(di) = player.deck().decode_info()
        && let Some(want) =
            native_rate_target(di.exclusive, di.decoded.sample_rate, di.device_rate, *tried)
    {
        if let Err(e) = player.deck_mut().reopen_sink_at(want) {
            *tried = Some(want);
            tracing::warn!(want_hz = want, error = ?e, "native-rate reopen failed");
            return;
        }
        // `reopen_sink_at` succeeds even when the device refused `want` and the sink
        // fell back to its previous rate, so check the rate we ACTUALLY landed on:
        // clear the latch only on a real switch, else latch `want` so we don't thrash.
        let landed = player.deck().decode_info().map(|d| d.device_rate);
        if landed == Some(want) {
            tracing::info!(
                rate_hz = want,
                "native-rate switch: device reopened bit-perfect"
            );
            *tried = None;
        } else {
            tracing::info!(
                want_hz = want,
                landed_hz = ?landed,
                "native-rate switch: device refused rate, resampling instead"
            );
            *tried = Some(want);
        }
    }
}

/// The desired output configuration — which device, whether to drive it exclusively
/// (bit-perfect), and (exclusive only) whether to STOP rather than fall back to shared.
/// The host updates this live; the pump applies it to the current track via
/// [`maybe_apply_output_change`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputConfig {
    /// The selected output device, or `None` for the system default.
    pub device: Option<String>,
    /// Drive the device exclusively (bypass the OS mixer, feed the DAC untouched).
    pub exclusive: bool,
    /// Exclusive-or-stop: don't fall back to shared on an exclusive failure.
    pub strict: bool,
}

/// Whether a desired output change needs the sink **rebuilt**. A **device** or
/// **exclusive-mode** change alters the hardware stream, so the sink must be swapped;
/// a **strict-only** change does not (it only affects how a FUTURE exclusive-open
/// handles a device refusal), so it's just recorded. Pure — the decision is unit-tested
/// without a device.
#[must_use]
pub fn output_change_needs_rebuild(current: &OutputConfig, desired: &OutputConfig) -> bool {
    current.device != desired.device || current.exclusive != desired.exclusive
}

/// The result of a live output-sink change, so the caller knows the deck's resulting
/// state (a bare `Result` can't say whether a failure left a WORKING sink or a silent
/// one). The caller latches `desired` on either failure variant (no per-block thrash).
pub enum OutputSwap {
    /// Now driving the desired device (`applied` was updated).
    Applied,
    /// The build failed but the deck was restored to a WORKING sink (the previous device,
    /// or the shared stream was never dropped) — audio continues.
    KeptCurrent(anyhow::Error),
    /// The build failed and the deck is on the SILENT park placeholder (strict stop, or
    /// even the restore couldn't open). The caller should treat the output as parked and
    /// recover (retry on a timer, or on a config change).
    Parked(anyhow::Error),
}

/// Apply a desired output config to the CURRENTLY playing track, live (not on the next
/// track; best practice: foobar/Roon apply immediately, accepting the brief hardware
/// reopen). If the device or exclusive mode changed, the swap order depends on whether
/// EXCLUSIVE is involved on either side:
///
/// - **Exclusive involved** (the current sink is exclusive, OR the desired mode is
///   exclusive) → release-then-acquire: `park` the deck on a device-free placeholder
///   (DROPS the old sink, freeing its device), then `build` the new sink on the now-free
///   device and swap it in. Exclusive WASAPI is one-client-per-device, so opening the
///   exclusive stream while ANY stream (the old exclusive one, or our own still-live
///   shared one) holds the same device intermittently fails with device-in-use. Freeing
///   the device first makes the exclusive open deterministic.
/// - **Shared to shared** (a plain device change, no exclusive) → build-then-swap:
///   `build` the new sink FIRST, while the old shared stream is still alive, then swap it
///   in (the old drops on swap). Shared mode allows multiple clients per device, so there
///   is no contention, and this avoids dropping a working shared stream only to re-open
///   on the same device, which some devices reject with "device no longer available".
///
/// `swap_sink` preserves playback position + ReplayGain. A strict-only change needs no
/// rebuild and is just recorded ([`OutputSwap::Applied`]).
///
/// On a `build` failure the return says where the deck ended: [`OutputSwap::KeptCurrent`]
/// (restored to a working sink, audio continues) or [`OutputSwap::Parked`] (silent — a
/// strict stop, or the restore itself failed). `applied` is left unchanged on failure;
/// the caller latches `desired` so it does not thrash the device every pump block.
pub fn maybe_apply_output_change<K, D, FBuild, FPark>(
    player: &mut ServicePlayer<K, D>,
    desired: &OutputConfig,
    applied: &mut OutputConfig,
    build: FBuild,
    park: FPark,
) -> OutputSwap
where
    K: Sink,
    D: SeekableDecoder,
    FBuild: Fn(&OutputConfig) -> anyhow::Result<K>,
    FPark: FnOnce() -> K,
{
    if !output_change_needs_rebuild(applied, desired) {
        *applied = desired.clone();
        return OutputSwap::Applied;
    }
    tracing::info!(from = ?applied, to = ?desired, "applying live output change");
    // Release-then-acquire whenever EXCLUSIVE is involved on either side: the current
    // sink is exclusive (it holds the device, one-client-per-device, so it must release
    // before anything else opens), OR the DESIRED mode is exclusive (opening the exclusive
    // stream while our OWN still-live shared stream holds the same device intermittently
    // fails with AUDCLNT_E_DEVICE_IN_USE: WASAPI sometimes lets exclusive preempt shared
    // and sometimes reports the device busy, which is exactly the flaky shared->exclusive
    // toggle). Dropping the current sink first frees the device so the exclusive open is
    // deterministic. A shared->shared device change still uses build-first below (no gap).
    if player.deck().sink().exclusive() || desired.exclusive {
        // Park on the device-free placeholder first (drops the old sink, frees the
        // device), then build the new sink on the freed device and swap it in.
        if let Err(e) = player.deck_mut().swap_sink(park()) {
            // Parking itself failed (the current track can't map onto the placeholder) —
            // the deck is untouched, still on its working sink.
            return OutputSwap::KeptCurrent(anyhow::anyhow!("park sink for output change: {e:?}"));
        }
        // The deck is now PARKED (silent); the old device is released.
        match build(desired) {
            Ok(new_sink) => match player.deck_mut().swap_sink(new_sink) {
                Ok(()) => {
                    *applied = desired.clone();
                    OutputSwap::Applied
                }
                Err(e) => OutputSwap::Parked(anyhow::anyhow!("swap_sink for output change: {e:?}")),
            },
            Err(e) => {
                // The new device wouldn't open. Under STRICT (exclusive-or-stop) we must
                // NOT fall back to the OS mixer or the old device: stay on the silent
                // placeholder so the stop is real. Otherwise try to re-acquire the previous
                // device so audio continues; if even THAT fails (e.g. another app now holds
                // the device, or cpal reports it gone after the release), stay parked.
                if desired.exclusive && desired.strict {
                    return OutputSwap::Parked(e);
                }
                match build(applied) {
                    Ok(old_sink) => {
                        if player.deck_mut().swap_sink(old_sink).is_ok() {
                            OutputSwap::KeptCurrent(e)
                        } else {
                            OutputSwap::Parked(e)
                        }
                    }
                    Err(_) => OutputSwap::Parked(e),
                }
            }
        }
    } else {
        // SHARED to SHARED (a plain device change, no exclusive). Build the new sink FIRST,
        // while the old shared stream is still alive, then swap it in (the old sink drops on
        // swap). Shared mode allows multiple clients per device, so the new open never
        // contends; and crucially this does NOT drop a working shared stream only to
        // immediately re-open on the same device, which some devices reject with "device no
        // longer available" (the release-then-acquire park path triggered exactly that). On
        // a build failure the old sink is untouched, so playback simply continues.
        match build(desired) {
            Ok(new_sink) => match player.deck_mut().swap_sink(new_sink) {
                Ok(()) => {
                    *applied = desired.clone();
                    OutputSwap::Applied
                }
                Err(e) => {
                    OutputSwap::KeptCurrent(anyhow::anyhow!("swap_sink for output change: {e:?}"))
                }
            },
            Err(e) => OutputSwap::KeptCurrent(e),
        }
    }
}

/// Rebuild the output sink from scratch for `desired` and swap it onto the deck,
/// **unconditionally** — used to recover from a parked (silent) sink after an earlier
/// build failed (e.g. a strict exclusive open that couldn't acquire the device, or no
/// device at all at startup). Unlike [`maybe_apply_output_change`] it does not compare
/// against `applied` (the live sink is the device-free placeholder, which holds no
/// device — so a plain build-then-swap is safe, nothing to release first) and it does
/// not restore an old device on failure (there is none; the deck stays on the silent
/// placeholder for the caller to retry).
///
/// # Errors
/// Propagates `build` failures (e.g. strict exclusive still can't open) so the caller
/// keeps the deck parked and may retry later or when the config changes.
pub fn rebuild_output_sink<K, D, FBuild>(
    player: &mut ServicePlayer<K, D>,
    desired: &OutputConfig,
    applied: &mut OutputConfig,
    build: FBuild,
) -> anyhow::Result<()>
where
    K: Sink,
    D: SeekableDecoder,
    FBuild: Fn(&OutputConfig) -> anyhow::Result<K>,
{
    let new_sink = build(desired)?;
    player
        .deck_mut()
        .swap_sink(new_sink)
        .map_err(|e| anyhow::anyhow!("swap_sink for output recovery: {e:?}"))?;
    *applied = desired.clone();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{CapturingSink, ConstDecoder, ExclusiveSink, NullSink};
    use crate::types::RepeatMode;
    use soul_audio_control::Player;
    use soul_audio_core::{Deck, Quality};
    use soul_audio_engine::Engine;

    const RATE: u32 = 48_000;
    const FRAMES: u64 = 4_000; // >> deck block, so a track spans several pumps

    /// A player over NullSink/ConstDecoder with a 1-second prev threshold.
    fn player() -> ServicePlayer<NullSink, ConstDecoder> {
        player_with_threshold(u64::from(RATE))
    }

    /// A player over NullSink/ConstDecoder with a custom prev threshold, so a
    /// short ConstDecoder track can deterministically outrun (or stay under)
    /// it — the 1 s default is unreachable on a 4000-frame track.
    fn player_with_threshold(prev_restart_frames: u64) -> ServicePlayer<NullSink, ConstDecoder> {
        let deck = Deck::new(NullSink::new(RATE, 2), Quality::Hq);
        let engine = Engine::new(deck, queue_for(Vec::new()));
        Player::new(engine, prev_restart_frames)
    }

    /// Run the open/pump loop `cycles` times, feeding a ConstDecoder for each
    /// track the player asks to open (id used as the constant sample so tracks
    /// are distinguishable). Deterministic — no threads, no device.
    fn drive(p: &mut ServicePlayer<NullSink, ConstDecoder>, cycles: usize) {
        for _ in 0..cycles {
            while let Some(id) = p.poll_open() {
                let dec = ConstDecoder::new(RATE, 2, FRAMES, id as i32 + 1);
                p.provide(id, dec).unwrap();
            }
            let _ = p.pump().unwrap();
        }
    }

    #[test]
    fn play_context_makes_the_start_track_current() {
        let mut p = player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0, 1, 2],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 3);
        assert_eq!(project_tick(&p, 1.0, false).now_playing_id, Some(0));
        assert_eq!(project_tick(&p, 1.0, false).state, PlayState::Playing);
    }

    #[test]
    fn pause_and_toggle_reflect_in_state() {
        let mut p = player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 3);
        apply_command(&mut p, Command::Pause, 0).unwrap();
        assert_eq!(project_tick(&p, 1.0, false).state, PlayState::Paused);
        apply_command(&mut p, Command::TogglePlay, 0).unwrap();
        assert_eq!(project_tick(&p, 1.0, false).state, PlayState::Playing);
    }

    #[test]
    fn play_next_then_enqueue_populate_upnext_in_order() {
        let mut p = player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0, 1, 2],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 3);
        apply_command(&mut p, Command::Enqueue(2), 0).unwrap();
        apply_command(&mut p, Command::PlayNext(1), 0).unwrap();
        // PlayNext jumps the front; Enqueue is behind it.
        assert_eq!(project_queue(&p).up_next_ids, vec![1, 2]);
        apply_command(&mut p, Command::ClearQueue, 0).unwrap();
        assert!(project_queue(&p).up_next_ids.is_empty());
    }

    #[test]
    fn queue_edit_commands_reorder_remove_and_jump() {
        let mut p = player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0, 1, 2, 3],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 3);
        for id in [10, 11, 12] {
            apply_command(&mut p, Command::Enqueue(id), 0).unwrap();
        }
        // Reorder the manual queue: move the front to index 2.
        apply_command(&mut p, Command::MoveInQueue { from: 0, to: 2 }, 0).unwrap();
        assert_eq!(project_queue(&p).up_next_ids, vec![11, 12, 10]);
        // Remove the item now at index 1.
        apply_command(&mut p, Command::RemoveFromQueue(1), 0).unwrap();
        assert_eq!(project_queue(&p).up_next_ids, vec![11, 10]);
        // The context band projects the REAL upcoming source remainder.
        assert_eq!(project_queue(&p).context_ids, vec![1, 2, 3]);
        // Click the first upcoming row (the queued 11): jump straight to it.
        apply_command(&mut p, Command::PlayUpcoming(1), 0).unwrap();
        drive(&mut p, 3);
        assert_eq!(project_tick(&p, 1.0, false).now_playing_id, Some(11));
    }

    /// A 5-track context playing track 10 at source position 0 (cur = 10).
    fn band_player() -> ServicePlayer<NullSink, ConstDecoder> {
        let mut p = player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![10, 20, 30, 40, 50],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 5);
        p
    }

    #[test]
    fn context_band_repeat_off_is_the_remainder() {
        // Baseline: Repeat::Off already terminates at the source end, so the band is
        // just the tracks after the current — no bounding needed, and the fix must
        // not disturb it.
        let mut p = band_player();
        apply_command(&mut p, Command::SetRepeat(RepeatMode::Off), 0).unwrap();
        assert_eq!(project_queue(&p).context_ids, vec![20, 30, 40, 50]);
    }

    #[test]
    fn context_band_repeat_all_is_one_pass_not_repeated_cycles() {
        // THE bug: upcoming_iter loops forever under Repeat::All, so the band must be
        // bounded to one pass over the OTHER source tracks — the current must not
        // re-appear, and no track may repeat.
        let mut p = band_player();
        apply_command(&mut p, Command::SetRepeat(RepeatMode::All), 0).unwrap();
        let ctx = project_queue(&p).context_ids;
        assert_eq!(
            ctx,
            vec![20, 30, 40, 50],
            "Repeat::All band is the album remainder once, not repeated cycles"
        );
        assert!(
            !ctx.contains(&10),
            "the current track must not re-appear upcoming"
        );
    }

    #[test]
    fn context_band_repeat_all_shuffle_has_no_duplicates() {
        // Same bound under shuffle: one pass over the four other tracks, no repeats.
        let mut p = band_player();
        apply_command(&mut p, Command::SetRepeat(RepeatMode::All), 0).unwrap();
        apply_command(&mut p, Command::SetShuffle(true), 777).unwrap();
        let ctx = project_queue(&p).context_ids;
        let mut uniq = ctx.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(
            ctx.len(),
            uniq.len(),
            "no duplicate tracks under shuffle+Repeat::All"
        );
        assert_eq!(ctx.len(), 4, "one pass over the other four source tracks");
        assert!(!ctx.contains(&10), "current excluded under shuffle too");
    }

    #[test]
    fn context_band_repeat_one_is_empty() {
        // Repeat::One freezes the source on the current track (upcoming_iter yields
        // the current forever) — so there is NO upcoming context to list.
        let mut p = band_player();
        apply_command(&mut p, Command::SetRepeat(RepeatMode::One), 0).unwrap();
        assert!(
            project_queue(&p).context_ids.is_empty(),
            "Repeat::One has no upcoming context band"
        );
    }

    #[test]
    fn signal_path_pcm_resample_path() {
        let mut p = player(); // NullSink @ 48_000
        assert!(project_signal_path(&p).is_none(), "nothing loaded → None");
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        // Provide a 44.1 kHz / 24-bit PCM decoder; pump so it becomes current.
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(44_100, 2, FRAMES, 7))
                .unwrap();
        }
        p.pump().unwrap();
        let sp = project_signal_path(&p).expect("loaded");
        assert_eq!(sp.source, SourceFormat::Pcm { bits: 24 });
        assert_eq!(sp.decoded_rate, 44_100);
        assert_eq!(sp.decoded_bits, 24);
        assert_eq!(sp.channels, 2);
        assert_eq!(sp.device_rate, 48_000);
        assert!(
            sp.resampling,
            "44.1 kHz decoder into a 48 kHz device resamples"
        );
        assert_eq!(
            sp.output_bits, 24,
            "NullSink reports canonical 24-bit as its output format"
        );
        assert!(
            !sp.exclusive,
            "the test NullSink is a shared sink, not exclusive"
        );
        assert_eq!(sp.replay_gain_db, 0.0, "no ReplayGain applied");
    }

    #[test]
    fn signal_path_matched_rate_is_not_resampling() {
        let mut p = player(); // NullSink @ 48_000
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(48_000, 2, FRAMES, 7))
                .unwrap();
        }
        p.pump().unwrap();
        let sp = project_signal_path(&p).unwrap();
        assert_eq!(sp.decoded_rate, 48_000);
        assert!(
            !sp.resampling,
            "48 kHz into a 48 kHz device: sample-rate-exact, no resample"
        );
    }

    // ── Native-rate switching (bit-perfect exclusive output) ──────────────────────

    /// A player over `sink` (a mock EXCLUSIVE sink) so `maybe_switch_native_rate` sees
    /// `exclusive() == true` and acts (a `NullSink` player is shared and never reopens).
    fn exclusive_player_with(sink: ExclusiveSink) -> ServicePlayer<ExclusiveSink, ConstDecoder> {
        let rate = sink.rate;
        let deck = Deck::new(sink, Quality::Hq);
        let engine = Engine::new(deck, queue_for(Vec::new()));
        Player::new(engine, u64::from(rate))
    }

    fn exclusive_player(rate: u32) -> ServicePlayer<ExclusiveSink, ConstDecoder> {
        exclusive_player_with(ExclusiveSink::new(rate, 2))
    }

    /// Play one `ConstDecoder` track at `track_rate` and pump once so it's current
    /// (and its decode→device path is projectable).
    fn load_one<K: Sink>(p: &mut ServicePlayer<K, ConstDecoder>, track_rate: u32) {
        apply_command(
            p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(track_rate, 2, FRAMES, 7))
                .unwrap();
        }
        p.pump().unwrap();
    }

    #[test]
    fn native_rate_target_only_reopens_an_exclusive_mismatched_untried_rate() {
        // Exclusive + the track's rate differs from the device → reopen at the track's
        // rate (bit-perfect).
        assert_eq!(native_rate_target(true, 44_100, 48_000, None), Some(44_100));
        // Shared sink (not exclusive) never reopens — it stays at the device rate and
        // resamples, so a shared user never thrashes the device.
        assert_eq!(native_rate_target(false, 44_100, 48_000, None), None);
        // Already rate-matched → nothing to do.
        assert_eq!(native_rate_target(true, 48_000, 48_000, None), None);
        // A rate the device just refused (recorded in `tried`) isn't retried — the
        // anti-loop / anti-thrash guard.
        assert_eq!(native_rate_target(true, 44_100, 48_000, Some(44_100)), None);
        // But a DIFFERENT rate than the one that failed is tried again.
        assert_eq!(
            native_rate_target(true, 96_000, 48_000, Some(44_100)),
            Some(96_000)
        );
    }

    #[test]
    fn native_rate_switch_reopens_exclusive_at_the_tracks_rate() {
        // ExclusiveSink @ 48k; a 44.1k track is exclusive but RESAMPLED. The switch
        // reopens the SAME device IN PLACE at the track's 44.1k → bit-perfect (no
        // resample), still exclusive — the whole point of the feature, end to end.
        let mut p = exclusive_player(48_000);
        load_one(&mut p, 44_100);
        let before = project_signal_path(&p).expect("loaded");
        assert!(
            before.exclusive && before.resampling && before.device_rate == 48_000,
            "before: exclusive but resampling 44.1→48, got {before:?}"
        );

        let mut tried = None;
        maybe_switch_native_rate(&mut p, &mut tried);

        let after = project_signal_path(&p).expect("still loaded");
        assert_eq!(
            after.device_rate, 44_100,
            "device reopened in place at the track's rate"
        );
        assert!(
            !after.resampling,
            "bit-perfect — no resample after the reopen"
        );
        assert!(after.exclusive, "still exclusive after the reopen");
        assert_eq!(tried, None, "a successful switch clears the retry latch");
    }

    /// Regression for "some tracks play ~2× slow / distorted on bit-perfect output":
    /// an implicit HE-AAC (SBR) track reports its AAC core rate (24k) at load, then
    /// flips to the 2× SBR rate (48k) after the first frame. On an exclusive device the
    /// native-rate switch reopens to the live 48k — and the deck must rebuild its
    /// resampler at the live rate too, else the stale 24k→48k resampler upsamples
    /// already-48k content 2× (half speed). After the switch the path must be
    /// bit-perfect (no resampling) at the live rate. With the stale-rate bug
    /// `resampling` stays true even though `decoded == device`.
    #[test]
    fn an_sbr_rate_flip_stays_bit_perfect_on_exclusive() {
        let mut p = exclusive_player(44_100); // exclusive device defaulting to 44.1k
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            // Core 24k → SBR 48k once the first frame is decoded.
            p.provide(id, ConstDecoder::new_sbr(24_000, 48_000, 2, FRAMES, 7))
                .unwrap();
        }
        p.pump().unwrap(); // first decode confirms SBR → the decoder now reports 48k

        let mut tried = None;
        maybe_switch_native_rate(&mut p, &mut tried);

        let sp = project_signal_path(&p).expect("loaded");
        assert_eq!(sp.decoded_rate, 48_000, "the live (post-SBR) decode rate");
        assert_eq!(sp.device_rate, 48_000, "reopened to the live rate");
        assert!(
            !sp.resampling,
            "bit-perfect after the SBR flip — no stale resampler desyncing speed, got {sp:?}"
        );
        assert_eq!(tried, None, "a successful switch clears the retry latch");
    }

    /// Changing the output config (device or exclusive mode) must apply to the
    /// CURRENTLY playing track — rebuild the sink in place (`swap_sink`, which keeps
    /// playback position + ReplayGain) and keep playing — not wait for the next track.
    /// Best practice (foobar/Roon): apply live, accept the brief hardware reopen; only
    /// "applies next session" when nothing is playing.
    #[test]
    fn an_output_config_change_applies_live_and_keeps_playing() {
        use crate::testutil::ConfigSink;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // The pure decision: a device OR exclusive change needs a rebuild; strict alone
        // does not (it only changes how a FUTURE exclusive-open handles a refusal).
        let shared = OutputConfig {
            device: None,
            exclusive: false,
            strict: false,
        };
        assert!(!output_change_needs_rebuild(&shared, &shared));
        assert!(output_change_needs_rebuild(
            &shared,
            &OutputConfig {
                exclusive: true,
                ..shared.clone()
            }
        ));
        assert!(output_change_needs_rebuild(
            &shared,
            &OutputConfig {
                device: Some("DAC".into()),
                ..shared.clone()
            }
        ));
        assert!(!output_change_needs_rebuild(
            &shared,
            &OutputConfig {
                strict: true,
                ..shared.clone()
            }
        ));

        let frames = Arc::new(AtomicUsize::new(0));
        let frames_c = Arc::clone(&frames);
        let build = move |cfg: &OutputConfig| -> anyhow::Result<ConfigSink> {
            Ok(ConfigSink::new(
                cfg.device.clone(),
                cfg.exclusive,
                48_000,
                2,
                Arc::clone(&frames_c),
            ))
        };

        let mut applied = shared.clone();
        let deck = Deck::new(build(&applied).unwrap(), Quality::Hq);
        let engine = Engine::new(deck, queue_for(Vec::new()));
        let mut p: ServicePlayer<ConfigSink, ConstDecoder> = Player::new(engine, 48_000);
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(48_000, 2, FRAMES, 7))
                .unwrap();
        }
        for _ in 0..3 {
            p.pump().unwrap();
        }
        assert!(!p.deck().sink().exclusive, "starts shared");
        let pos_before = project_tick(&p, 1.0, false).position_secs;
        let written_before = frames.load(Ordering::Relaxed);
        assert!(written_before > 0, "playing before the change");

        // Flip to EXCLUSIVE live.
        let desired = OutputConfig {
            device: None,
            exclusive: true,
            strict: false,
        };
        let park = || ConfigSink::new(None, false, 48_000, 2, Arc::clone(&frames));
        assert!(matches!(
            maybe_apply_output_change(&mut p, &desired, &mut applied, &build, park),
            OutputSwap::Applied
        ));

        assert!(
            p.deck().sink().exclusive,
            "exclusive applied LIVE to the current track, not next session"
        );
        assert_eq!(
            p.deck().sink().device,
            None,
            "the exclusive flip didn't disturb the device selection"
        );
        assert_eq!(applied, desired, "applied config tracks the desired one");
        let pos_after = project_tick(&p, 1.0, false).position_secs;
        assert!(
            pos_after >= pos_before,
            "playback position preserved across the live swap (no restart): {pos_after} < {pos_before}"
        );
        for _ in 0..3 {
            p.pump().unwrap();
        }
        assert!(
            frames.load(Ordering::Relaxed) > written_before,
            "playback keeps flowing through the rebuilt sink"
        );
    }

    /// RELEASE-THEN-ACQUIRE (current sink is EXCLUSIVE): the OLD sink must be dropped
    /// (its device freed) BEFORE the new sink is built — exclusive WASAPI is
    /// one-client-per-device, so opening the new stream while the old still held the
    /// same device fails with device-in-use. We count live "device holders"; the build
    /// closure records how many are live the instant it runs. A correct release-first
    /// swap sees ZERO.
    #[test]
    fn an_exclusive_output_change_releases_the_old_device_before_opening_the_new() {
        use crate::testutil::ConfigSink;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let frames = Arc::new(AtomicUsize::new(0));
        let device_live = Arc::new(AtomicUsize::new(0));
        let live_at_build = Arc::new(AtomicUsize::new(usize::MAX));

        let build = {
            let frames = Arc::clone(&frames);
            let device_live = Arc::clone(&device_live);
            let live_at_build = Arc::clone(&live_at_build);
            move |cfg: &OutputConfig| -> anyhow::Result<ConfigSink> {
                // How many old devices are still live as we open the new one?
                live_at_build.store(device_live.load(Ordering::Relaxed), Ordering::Relaxed);
                Ok(ConfigSink::new_counting(
                    cfg.device.clone(),
                    cfg.exclusive,
                    48_000,
                    2,
                    Arc::clone(&frames),
                    Arc::clone(&device_live),
                ))
            }
        };
        // The parked placeholder holds no device, so it never counts.
        let park = {
            let frames = Arc::clone(&frames);
            move || ConfigSink::new(None, false, 48_000, 2, Arc::clone(&frames))
        };

        // Current sink is EXCLUSIVE — the case that needs release-then-acquire.
        let mut applied = OutputConfig {
            device: None,
            exclusive: true,
            strict: false,
        };
        let deck = Deck::new(build(&applied).unwrap(), Quality::Hq); // device_live -> 1
        let engine = Engine::new(deck, queue_for(Vec::new()));
        let mut p: ServicePlayer<ConfigSink, ConstDecoder> = Player::new(engine, 48_000);
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(48_000, 2, FRAMES, 7))
                .unwrap();
        }
        for _ in 0..3 {
            p.pump().unwrap();
        }
        assert_eq!(
            device_live.load(Ordering::Relaxed),
            1,
            "one device open while playing"
        );

        // Switch the exclusive output to a different device — needs release-first.
        live_at_build.store(usize::MAX, Ordering::Relaxed); // reset the probe
        let desired = OutputConfig {
            device: Some("DAC".into()),
            exclusive: true,
            strict: false,
        };
        assert!(matches!(
            maybe_apply_output_change(&mut p, &desired, &mut applied, &build, park),
            OutputSwap::Applied
        ));

        assert_eq!(
            live_at_build.load(Ordering::Relaxed),
            0,
            "the old exclusive device must be RELEASED before the new sink opens"
        );
        assert_eq!(
            device_live.load(Ordering::Relaxed),
            1,
            "exactly one device live after the swap (old released, new open)"
        );
        assert_eq!(
            p.deck().sink().device,
            Some("DAC".into()),
            "the new exclusive sink is installed on the new device"
        );
    }

    /// THE flaky shared->exclusive toggle fix: opening the exclusive stream while our OWN
    /// shared stream still holds the device intermittently hits device-in-use. So a change
    /// whose DESIRED mode is exclusive must ALSO release-then-acquire (drop the current
    /// shared sink BEFORE building the exclusive one), even though the current sink isn't
    /// exclusive. We assert ZERO live device holders at the instant the exclusive sink is
    /// built — the old build-first path left the shared sink live (count 1) and raced.
    #[test]
    fn a_shared_to_exclusive_change_releases_the_shared_device_before_opening_exclusive() {
        use crate::testutil::ConfigSink;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let frames = Arc::new(AtomicUsize::new(0));
        let device_live = Arc::new(AtomicUsize::new(0));
        let live_at_build = Arc::new(AtomicUsize::new(usize::MAX));

        let build = {
            let frames = Arc::clone(&frames);
            let device_live = Arc::clone(&device_live);
            let live_at_build = Arc::clone(&live_at_build);
            move |cfg: &OutputConfig| -> anyhow::Result<ConfigSink> {
                live_at_build.store(device_live.load(Ordering::Relaxed), Ordering::Relaxed);
                Ok(ConfigSink::new_counting(
                    cfg.device.clone(),
                    cfg.exclusive,
                    48_000,
                    2,
                    Arc::clone(&frames),
                    Arc::clone(&device_live),
                ))
            }
        };
        let park = {
            let frames = Arc::clone(&frames);
            move || ConfigSink::new(None, false, 48_000, 2, Arc::clone(&frames))
        };

        // Current sink is SHARED (exclusive = false).
        let mut applied = OutputConfig {
            device: None,
            exclusive: false,
            strict: false,
        };
        let deck = Deck::new(build(&applied).unwrap(), Quality::Hq); // device_live -> 1
        let engine = Engine::new(deck, queue_for(Vec::new()));
        let mut p: ServicePlayer<ConfigSink, ConstDecoder> = Player::new(engine, 48_000);
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(48_000, 2, FRAMES, 7))
                .unwrap();
        }
        for _ in 0..3 {
            p.pump().unwrap();
        }
        assert_eq!(
            device_live.load(Ordering::Relaxed),
            1,
            "one shared device open while playing"
        );

        // Toggle to EXCLUSIVE on the same device: must free the shared device first.
        live_at_build.store(usize::MAX, Ordering::Relaxed);
        let desired = OutputConfig {
            device: None,
            exclusive: true,
            strict: false,
        };
        assert!(matches!(
            maybe_apply_output_change(&mut p, &desired, &mut applied, &build, park),
            OutputSwap::Applied
        ));

        assert_eq!(
            live_at_build.load(Ordering::Relaxed),
            0,
            "the shared device must be RELEASED before the exclusive stream opens (no device-in-use race)"
        );
        assert_eq!(
            device_live.load(Ordering::Relaxed),
            1,
            "exactly one device live after the swap (shared released, exclusive open)"
        );
        assert!(
            p.deck().sink().exclusive,
            "now driving the device exclusively"
        );
    }

    /// NO-GAP for SHARED (the "device no longer available" regression): when the current
    /// sink is SHARED, a change must NOT drop the old stream before opening the new one —
    /// some devices reject an immediate same-device re-open. The new sink opens WHILE the
    /// old is still alive, so at build time the old device is still a live holder.
    #[test]
    fn a_shared_output_change_opens_the_new_before_dropping_the_old() {
        use crate::testutil::ConfigSink;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let frames = Arc::new(AtomicUsize::new(0));
        let device_live = Arc::new(AtomicUsize::new(0));
        let live_at_build = Arc::new(AtomicUsize::new(usize::MAX));

        let build = {
            let frames = Arc::clone(&frames);
            let device_live = Arc::clone(&device_live);
            let live_at_build = Arc::clone(&live_at_build);
            move |cfg: &OutputConfig| -> anyhow::Result<ConfigSink> {
                live_at_build.store(device_live.load(Ordering::Relaxed), Ordering::Relaxed);
                Ok(ConfigSink::new_counting(
                    cfg.device.clone(),
                    cfg.exclusive,
                    48_000,
                    2,
                    Arc::clone(&frames),
                    Arc::clone(&device_live),
                ))
            }
        };
        // Park must NOT be used on the shared path; if it were, the old would drop first.
        let park = {
            let frames = Arc::clone(&frames);
            move || ConfigSink::new(None, false, 48_000, 2, Arc::clone(&frames))
        };

        // Current sink is SHARED (exclusive = false).
        let mut applied = OutputConfig {
            device: None,
            exclusive: false,
            strict: false,
        };
        let deck = Deck::new(build(&applied).unwrap(), Quality::Hq); // device_live -> 1
        let engine = Engine::new(deck, queue_for(Vec::new()));
        let mut p: ServicePlayer<ConfigSink, ConstDecoder> = Player::new(engine, 48_000);
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(48_000, 2, FRAMES, 7))
                .unwrap();
        }
        for _ in 0..3 {
            p.pump().unwrap();
        }

        // Switch to a different SHARED device.
        live_at_build.store(usize::MAX, Ordering::Relaxed);
        let desired = OutputConfig {
            device: Some("Speakers".into()),
            exclusive: false,
            strict: false,
        };
        assert!(matches!(
            maybe_apply_output_change(&mut p, &desired, &mut applied, &build, park),
            OutputSwap::Applied
        ));

        assert_eq!(
            live_at_build.load(Ordering::Relaxed),
            1,
            "the new shared sink opens while the OLD is still alive — no device-release gap"
        );
        assert_eq!(
            device_live.load(Ordering::Relaxed),
            1,
            "old dropped after the swap — exactly one live device"
        );
        assert_eq!(
            p.deck().sink().device,
            Some("Speakers".into()),
            "the new shared device is installed"
        );
    }

    /// A FAILED output change must keep playback alive on the ORIGINAL device — not die
    /// on the silent placeholder — and report the error so the pump can latch it (no
    /// retry thrash).
    #[test]
    fn a_failed_output_change_restores_the_old_device_and_keeps_playing() {
        use crate::testutil::ConfigSink;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let frames = Arc::new(AtomicUsize::new(0));
        let fail_exclusive = Arc::new(AtomicBool::new(true));
        let build = {
            let frames = Arc::clone(&frames);
            let fail_exclusive = Arc::clone(&fail_exclusive);
            move |cfg: &OutputConfig| -> anyhow::Result<ConfigSink> {
                if cfg.exclusive && fail_exclusive.load(Ordering::Relaxed) {
                    anyhow::bail!("device in use (test)");
                }
                Ok(ConfigSink::new(
                    cfg.device.clone(),
                    cfg.exclusive,
                    48_000,
                    2,
                    Arc::clone(&frames),
                ))
            }
        };
        let park = {
            let frames = Arc::clone(&frames);
            move || ConfigSink::new(None, false, 48_000, 2, Arc::clone(&frames))
        };

        let shared = OutputConfig {
            device: None,
            exclusive: false,
            strict: false,
        };
        let mut applied = shared.clone();
        let deck = Deck::new(build(&applied).unwrap(), Quality::Hq);
        let engine = Engine::new(deck, queue_for(Vec::new()));
        let mut p: ServicePlayer<ConfigSink, ConstDecoder> = Player::new(engine, 48_000);
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(48_000, 2, FRAMES, 7))
                .unwrap();
        }
        for _ in 0..3 {
            p.pump().unwrap();
        }
        let written_before = frames.load(Ordering::Relaxed);

        // Try to flip to exclusive — which can't open.
        let desired = OutputConfig {
            device: None,
            exclusive: true,
            strict: false,
        };
        let res = maybe_apply_output_change(&mut p, &desired, &mut applied, &build, park);
        assert!(
            matches!(res, OutputSwap::KeptCurrent(_)),
            "a non-strict exclusive open failure keeps the current (shared) device playing"
        );
        assert!(
            !p.deck().sink().exclusive,
            "stayed on the original shared device"
        );
        assert_eq!(
            applied, shared,
            "applied is unchanged on failure (pump can latch desired)"
        );

        // Playback must keep flowing on the restored device.
        for _ in 0..3 {
            p.pump().unwrap();
        }
        assert!(
            frames.load(Ordering::Relaxed) > written_before,
            "playback continues on the restored original device"
        );
    }

    /// STRICT (exclusive-or-stop): when a live change to strict exclusive can't open the
    /// device, it must NOT keep the OS-mixer (shared) stream playing — it goes SILENT
    /// (parks on the device-free placeholder) so the stop is real and the Settings card's
    /// "Stopped" status is truthful. The non-strict case is the opposite (keeps playing),
    /// covered by `a_failed_output_change_restores_the_old_device_and_keeps_playing`.
    #[test]
    fn a_strict_exclusive_change_that_fails_goes_silent_not_shared() {
        use crate::testutil::ConfigSink;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let frames = Arc::new(AtomicUsize::new(0));
        // Exclusive never opens; the park placeholder is tagged so we can see the deck
        // land on it (vs. keeping the old shared None-device sink).
        let build = {
            let frames = Arc::clone(&frames);
            move |cfg: &OutputConfig| -> anyhow::Result<ConfigSink> {
                if cfg.exclusive {
                    anyhow::bail!("device has no exclusive format (test)");
                }
                Ok(ConfigSink::new(
                    cfg.device.clone(),
                    false,
                    48_000,
                    2,
                    Arc::clone(&frames),
                ))
            }
        };
        let park = {
            let frames = Arc::clone(&frames);
            move || {
                ConfigSink::new(
                    Some("__silent__".into()),
                    false,
                    48_000,
                    2,
                    Arc::clone(&frames),
                )
            }
        };

        let mut applied = OutputConfig {
            device: None,
            exclusive: false,
            strict: false,
        };
        let deck = Deck::new(build(&applied).unwrap(), Quality::Hq); // starts shared
        let engine = Engine::new(deck, queue_for(Vec::new()));
        let mut p: ServicePlayer<ConfigSink, ConstDecoder> = Player::new(engine, 48_000);
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(48_000, 2, FRAMES, 7))
                .unwrap();
        }
        for _ in 0..3 {
            p.pump().unwrap();
        }

        // Flip to STRICT exclusive — which can't open.
        let desired = OutputConfig {
            device: None,
            exclusive: true,
            strict: true,
        };
        let res = maybe_apply_output_change(&mut p, &desired, &mut applied, &build, park);
        assert!(
            matches!(res, OutputSwap::Parked(_)),
            "strict exclusive that can't open parks the deck (silent), reporting the reason"
        );
        assert_eq!(
            p.deck().sink().device,
            Some("__silent__".into()),
            "strict stop goes SILENT (parks), rather than keeping the OS-mixer shared stream"
        );
        assert!(
            !p.deck().sink().exclusive,
            "the silent placeholder is not an exclusive device"
        );
    }

    /// Recovery: a deck parked on the silent placeholder (a build failed) rebuilds to a
    /// real device sink via `rebuild_output_sink` and keeps the playback position —
    /// unlike `maybe_apply_output_change`, it force-builds even when `applied == desired`
    /// (the live sink is the placeholder, not `applied`'s device).
    #[test]
    fn rebuild_output_sink_recovers_a_parked_deck() {
        use crate::testutil::ConfigSink;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        let frames = Arc::new(AtomicUsize::new(0));
        let build = {
            let frames = Arc::clone(&frames);
            move |cfg: &OutputConfig| -> anyhow::Result<ConfigSink> {
                Ok(ConfigSink::new(
                    cfg.device.clone(),
                    cfg.exclusive,
                    48_000,
                    2,
                    Arc::clone(&frames),
                ))
            }
        };

        // Deck starts PARKED on the silent placeholder (as the pump does after an initial
        // build failure).
        let deck = Deck::new(
            ConfigSink::new(
                Some("__silent__".into()),
                false,
                48_000,
                2,
                Arc::clone(&frames),
            ),
            Quality::Hq,
        );
        let engine = Engine::new(deck, queue_for(Vec::new()));
        let mut p: ServicePlayer<ConfigSink, ConstDecoder> = Player::new(engine, 48_000);
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        while let Some(id) = p.poll_open() {
            p.provide(id, ConstDecoder::new(48_000, 2, FRAMES, 7))
                .unwrap();
        }
        for _ in 0..3 {
            p.pump().unwrap();
        }
        let pos_before = project_tick(&p, 1.0, false).position_secs;
        assert_eq!(
            p.deck().sink().device,
            Some("__silent__".into()),
            "starts parked on the placeholder"
        );

        // Recover to the real default device.
        let desired = OutputConfig {
            device: Some("Speakers".into()),
            exclusive: false,
            strict: false,
        };
        let mut applied = OutputConfig::default();
        rebuild_output_sink(&mut p, &desired, &mut applied, &build).unwrap();

        assert_eq!(
            p.deck().sink().device,
            Some("Speakers".into()),
            "recovered onto the real device"
        );
        assert_eq!(applied, desired, "applied tracks the recovered config");
        assert!(
            project_tick(&p, 1.0, false).position_secs >= pos_before,
            "recovery preserves the playback position (no restart)"
        );
    }

    /// `rebuild_output_sink` stays parked (and errors) until the build can succeed — the
    /// transient-failure retry contract the pump leans on (device momentarily in use).
    #[test]
    fn rebuild_output_sink_stays_parked_until_the_build_succeeds() {
        use crate::testutil::ConfigSink;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let frames = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(true));
        let build = {
            let frames = Arc::clone(&frames);
            let fail = Arc::clone(&fail);
            move |cfg: &OutputConfig| -> anyhow::Result<ConfigSink> {
                if fail.load(Ordering::Relaxed) {
                    anyhow::bail!("device in use (test)");
                }
                Ok(ConfigSink::new(
                    cfg.device.clone(),
                    cfg.exclusive,
                    48_000,
                    2,
                    Arc::clone(&frames),
                ))
            }
        };

        let deck = Deck::new(
            ConfigSink::new(
                Some("__silent__".into()),
                false,
                48_000,
                2,
                Arc::clone(&frames),
            ),
            Quality::Hq,
        );
        let engine = Engine::new(deck, queue_for(Vec::new()));
        let mut p: ServicePlayer<ConfigSink, ConstDecoder> = Player::new(engine, 48_000);
        let desired = OutputConfig {
            device: Some("DAC".into()),
            exclusive: true,
            strict: true,
        };
        let mut applied = OutputConfig::default();

        // While the device is "in use", recovery fails and the deck stays silent.
        assert!(rebuild_output_sink(&mut p, &desired, &mut applied, &build).is_err());
        assert_eq!(
            p.deck().sink().device,
            Some("__silent__".into()),
            "still parked while the build fails"
        );

        // The blocker clears → the next retry recovers.
        fail.store(false, Ordering::Relaxed);
        rebuild_output_sink(&mut p, &desired, &mut applied, &build).unwrap();
        assert_eq!(
            p.deck().sink().device,
            Some("DAC".into()),
            "recovered once the device freed"
        );
        assert!(
            p.deck().sink().exclusive,
            "recovered as the requested exclusive device"
        );
    }

    #[test]
    fn native_rate_switch_leaves_a_shared_sink_untouched() {
        // A shared sink (NullSink, exclusive() == false) must NEVER reopen — shared
        // users stay at the device rate and resample, with no device thrash.
        let mut p = player(); // NullSink @ 48k (shared)
        load_one(&mut p, 44_100);
        let mut tried = None;
        maybe_switch_native_rate(&mut p, &mut tried);
        let sp = project_signal_path(&p).unwrap();
        assert!(
            sp.resampling && sp.device_rate == 48_000,
            "shared stays at the device rate and resamples"
        );
        assert_eq!(tried, None, "a shared sink never latches a reopen");
    }

    #[test]
    fn native_rate_switch_is_a_noop_when_already_rate_matched() {
        // Exclusive sink @ 48k with a 48k track: already bit-perfect, so no reopen.
        let sink = ExclusiveSink::new(48_000, 2);
        let reopens = std::sync::Arc::clone(&sink.reopens);
        let mut p = exclusive_player_with(sink);
        load_one(&mut p, 48_000);
        assert!(
            !project_signal_path(&p).unwrap().resampling,
            "already matched"
        );
        let mut tried = None;
        maybe_switch_native_rate(&mut p, &mut tried);
        assert_eq!(
            reopens.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no reopen when the rate already matches"
        );
    }

    #[test]
    fn native_rate_switch_latches_a_refused_rate_and_does_not_thrash() {
        // The device REFUSES exclusive at the track's rate, so the in-place reopen
        // falls back to the current rate (the real sink does this). The first call
        // attempts ONCE and keeps the current exclusive sink (resampling); the latch
        // then suppresses retrying every pump — the anti-thrash guard.
        let sink = ExclusiveSink::refusing(48_000, 2);
        let reopens = std::sync::Arc::clone(&sink.reopens);
        let mut p = exclusive_player_with(sink);
        load_one(&mut p, 44_100);

        let mut tried = None;
        maybe_switch_native_rate(&mut p, &mut tried);
        assert_eq!(
            reopens.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "one reopen attempt"
        );
        assert_eq!(tried, Some(44_100), "the refused rate is latched");
        let kept = project_signal_path(&p).unwrap();
        assert!(
            kept.exclusive && kept.resampling && kept.device_rate == 48_000,
            "a refused reopen keeps the current exclusive sink (resampling), not shared"
        );

        // A later pump at the same rate must NOT re-attempt — no device thrash.
        maybe_switch_native_rate(&mut p, &mut tried);
        assert_eq!(
            reopens.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the latched rate is not retried"
        );
    }

    #[test]
    fn native_rate_switches_per_track_across_a_mixed_rate_sequence() {
        // 44.1k track → reopen at 44.1k (bit-perfect); then a 96k track → reopen at
        // 96k. A success after an earlier switch clears the latch, so each new rate
        // switches. Models an album boundary where the rate changes.
        let mut p = exclusive_player(48_000);
        let mut tried = None;

        load_one(&mut p, 44_100);
        maybe_switch_native_rate(&mut p, &mut tried);
        let a = project_signal_path(&p).unwrap();
        assert_eq!(
            (a.device_rate, a.resampling),
            (44_100, false),
            "first track bit-perfect"
        );

        // Play a second track at a new rate (clears preloads + loads it current).
        load_one(&mut p, 96_000);
        let mid = project_signal_path(&p).unwrap();
        assert!(
            mid.resampling,
            "96k into the 44.1k device resamples until the switch"
        );
        maybe_switch_native_rate(&mut p, &mut tried);
        let b = project_signal_path(&p).unwrap();
        assert_eq!(
            (b.device_rate, b.resampling),
            (96_000, false),
            "second track bit-perfect"
        );
        assert!(b.exclusive);
    }

    #[test]
    fn shuffle_and_repeat_flags_project() {
        let mut p = player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0, 1, 2, 3],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 3);
        apply_command(&mut p, Command::SetShuffle(true), 12345).unwrap();
        assert!(project_tick(&p, 1.0, false).shuffle);
        apply_command(&mut p, Command::SetRepeat(RepeatMode::All), 0).unwrap();
        assert_eq!(project_tick(&p, 1.0, false).repeat, RepeatMode::All);
        apply_command(&mut p, Command::SetShuffle(false), 0).unwrap();
        assert!(!project_tick(&p, 1.0, false).shuffle);
    }

    #[test]
    fn prev_restarts_when_past_threshold_else_steps_back() {
        // Use a small threshold (FRAMES/4 = 1000 frames) so the 4000-frame
        // ConstDecoder track can both outrun it (restart branch) and sit under
        // it (step-back branch). Advance to track 1 and play past the threshold,
        // then Prev must restart track 1 (seek 0, cursor stays).
        let mut p = player_with_threshold(FRAMES / 4);
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0, 1, 2],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 3);
        apply_command(&mut p, Command::Next, 0).unwrap();
        drive(&mut p, 2);
        assert_eq!(p.current(), Some(1), "on track 1");
        // Advance enough pumps (BLOCK=256 frames each) to exceed FRAMES/4.
        drive(&mut p, 4);
        assert!(
            p.position_frames() > FRAMES / 4,
            "past threshold: pos {} > {}",
            p.position_frames(),
            FRAMES / 4
        );
        // Past threshold → Prev restarts track 1 (seek 0, cursor stays).
        apply_command(&mut p, Command::Prev, 0).unwrap();
        assert_eq!(
            p.current(),
            Some(1),
            "prev past threshold restarts on track 1"
        );
        assert_eq!(p.position_frames(), 0, "restart resets position");
        // Now at the track start (under threshold) → Prev steps back to track 0.
        apply_command(&mut p, Command::Prev, 0).unwrap();
        assert_eq!(p.current(), Some(0), "prev under threshold steps back");
    }

    #[test]
    fn goto_jumps_within_context() {
        let mut p = player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0, 1, 2, 3],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 3);
        apply_command(&mut p, Command::Goto(2), 0).unwrap();
        drive(&mut p, 3);
        assert_eq!(p.current(), Some(2));
    }

    // --- output-stage effects (via a CapturingSink) ------------------------
    // The tests above assert projected STATE; these assert the actual effect a
    // command has on the audio the deck writes — the deterministic, device-free
    // mirror of what a listener hears (no cpal, so the architecture gate holds).

    fn cap_player() -> ServicePlayer<CapturingSink, ConstDecoder> {
        let deck = Deck::new(CapturingSink::new(RATE, 2), Quality::Hq);
        let engine = Engine::new(deck, queue_for(Vec::new()));
        Player::new(engine, u64::from(RATE))
    }

    /// Drive the open/pump loop `cycles` times, providing a `ConstDecoder` per
    /// opened id via `mk` (id → `(total_frames, const_value)`). Deterministic.
    fn drive_cap(
        p: &mut ServicePlayer<CapturingSink, ConstDecoder>,
        cycles: usize,
        mk: impl Fn(u32) -> (u64, i32),
    ) {
        for _ in 0..cycles {
            while let Some(id) = p.poll_open() {
                let (total, value) = mk(id);
                p.provide(id, ConstDecoder::new(RATE, 2, total, value))
                    .unwrap();
            }
            let _ = p.pump().unwrap();
        }
    }

    #[test]
    fn pause_halts_sink_output() {
        let mut p = cap_player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive_cap(&mut p, 5, |_| (FRAMES, 1_000_000)); // 5 blocks << 4000-frame track
        let before = p.deck().sink().written.len();
        assert!(before > 0, "played some audio before pausing");

        apply_command(&mut p, Command::Pause, 0).unwrap();
        drive_cap(&mut p, 20, |_| (FRAMES, 1_000_000));
        assert_eq!(
            p.deck().sink().written.len(),
            before,
            "no samples are written to the sink while paused"
        );
        assert_eq!(project_tick(&p, 1.0, false).state, PlayState::Paused);
    }

    #[test]
    fn stop_halts_sink_output_and_reports_stopped() {
        let mut p = cap_player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive_cap(&mut p, 5, |_| (FRAMES, 1_000_000));
        let before = p.deck().sink().written.len();
        assert!(before > 0);

        apply_command(&mut p, Command::Stop, 0).unwrap();
        drive_cap(&mut p, 20, |_| (FRAMES, 1_000_000));
        assert_eq!(
            p.deck().sink().written.len(),
            before,
            "no samples are written to the sink after stop"
        );
        assert_eq!(project_tick(&p, 1.0, false).state, PlayState::Stopped);
    }

    #[test]
    fn set_volume_attenuates_sink_output() {
        const V: i32 = 4_000_000; // within canonical 24-bit (±2^23 after norm)
        let mut p = cap_player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        // Long track so it never ends; settle at the default full volume.
        drive_cap(&mut p, 20, |_| (400_000, V));
        let full = *p.deck().sink().written.last().unwrap();
        assert!(full.abs() > 0, "non-silent at full volume");

        // Quarter volume, then drive long enough for the per-sample gain ramp to
        // settle (0.99^n → 0 well within 40 blocks).
        apply_command(&mut p, Command::SetVolume(0.25), 0).unwrap();
        drive_cap(&mut p, 40, |_| (400_000, V));
        let quiet = *p.deck().sink().written.last().unwrap();

        // The output-stage gain is the ONLY thing changed, so the ratio is the
        // volume ratio regardless of the deck's normalization factor.
        let ratio = f64::from(quiet) / f64::from(full);
        assert!(
            (ratio - 0.25).abs() < 0.03,
            "volume 0.25 should attenuate output to ~0.25× (got {ratio:.3})"
        );
    }

    // ── VolumeState: the mute/volume source of truth ──────────────────────────

    #[test]
    fn volume_state_defaults_to_full_and_unmuted() {
        let v = VolumeState::new();
        assert_eq!(v.volume(), 1.0);
        assert!(!v.muted());
        assert_eq!(v.gain(), 1.0);
        assert_eq!(v, VolumeState::default());
    }

    #[test]
    fn set_volume_sets_and_clamps_the_level() {
        let mut v = VolumeState::new();
        v.set_volume(0.3);
        assert_eq!(v.volume(), 0.3);
        assert_eq!(v.gain(), 0.3);
        v.set_volume(1.5);
        assert_eq!(v.volume(), 1.0, "clamped to 1.0");
        v.set_volume(-0.2);
        assert_eq!(v.volume(), 0.0, "clamped to 0.0");
        assert_eq!(v.gain(), 0.0);
    }

    #[test]
    fn toggle_mute_silences_the_gain_but_preserves_the_level() {
        let mut v = VolumeState::new();
        v.set_volume(0.6);
        v.toggle_mute();
        assert!(v.muted());
        assert_eq!(v.gain(), 0.0, "muted → silent gain");
        assert_eq!(v.volume(), 0.6, "the volume level is preserved across mute");
        v.toggle_mute();
        assert!(!v.muted());
        assert_eq!(v.gain(), 0.6, "un-mute restores the exact pre-mute gain");
    }

    #[test]
    fn a_positive_set_volume_clears_mute() {
        let mut v = VolumeState::new();
        v.toggle_mute();
        assert!(v.muted());
        v.set_volume(0.4); // dragging the slider up
        assert!(
            !v.muted(),
            "a positive volume un-mutes (the VolumeControl behaviour)"
        );
        assert_eq!(v.gain(), 0.4);
    }

    #[test]
    fn setting_volume_to_zero_is_a_level_not_a_mute() {
        let mut v = VolumeState::new();
        v.set_volume(0.0);
        assert!(
            !v.muted(),
            "volume 0 is a level, distinct from the mute flag"
        );
        assert_eq!(v.gain(), 0.0);
        // Mute then un-mute from a 0 level still yields 0 — no spurious restore.
        v.toggle_mute();
        assert_eq!(v.gain(), 0.0);
        v.toggle_mute();
        assert_eq!(v.gain(), 0.0);
        assert_eq!(v.volume(), 0.0);
    }

    #[test]
    fn mute_is_idempotent_per_toggle_and_round_trips() {
        let mut v = VolumeState::new();
        v.set_volume(0.75);
        for _ in 0..3 {
            v.toggle_mute();
            assert!(v.muted());
            assert_eq!(v.gain(), 0.0);
            v.toggle_mute();
            assert!(!v.muted());
            assert_eq!(
                v.gain(),
                0.75,
                "each round trip returns to the pre-mute level"
            );
        }
    }

    #[test]
    fn muting_silences_the_sink_and_unmuting_restores_it() {
        const V: i32 = 4_000_000;
        let mut p = cap_player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        // Half volume via the state's effective gain; settle the ramp.
        let mut vol = VolumeState::new();
        vol.set_volume(0.5);
        apply_command(&mut p, Command::SetVolume(vol.gain()), 0).unwrap();
        drive_cap(&mut p, 40, |_| (400_000, V));
        let playing = *p.deck().sink().written.last().unwrap();
        assert!(playing.abs() > 0, "audible before mute");

        // Mute → effective gain 0 → the sink ramps to silence.
        vol.toggle_mute();
        apply_command(&mut p, Command::SetVolume(vol.gain()), 0).unwrap();
        drive_cap(&mut p, 60, |_| (400_000, V));
        let muted = *p.deck().sink().written.last().unwrap();
        let muted_ratio = f64::from(muted.abs()) / f64::from(playing.abs());
        assert!(
            muted_ratio < 0.02,
            "muted output is effectively silent (got {muted_ratio:.4})"
        );

        // Un-mute → restores the pre-mute level.
        vol.toggle_mute();
        apply_command(&mut p, Command::SetVolume(vol.gain()), 0).unwrap();
        drive_cap(&mut p, 60, |_| (400_000, V));
        let restored = *p.deck().sink().written.last().unwrap();
        let restored_ratio = f64::from(restored) / f64::from(playing);
        assert!(
            (restored_ratio - 1.0).abs() < 0.05,
            "un-mute returns to the pre-mute level (got {restored_ratio:.3})"
        );
    }

    #[test]
    fn project_tick_reports_the_mute_flag() {
        let p = player(); // any player; `muted` is passed in, not read off it
        assert!(!project_tick(&p, 0.8, false).muted);
        let t = project_tick(&p, 0.8, true);
        assert!(t.muted, "Tick carries the mute flag");
        assert_eq!(
            t.volume, 0.8,
            "and the preserved volume, independent of mute"
        );
    }

    #[test]
    fn gapless_track_change_keeps_output_contiguous() {
        let mut p = cap_player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0, 1],
                start: 0,
            },
            0,
        )
        .unwrap();
        // Track 0 is a short 600-frame constant; it ends and must GAPLESSLY hand
        // over to track 1 (a different constant) with no inserted silence/fade.
        let mk = |id: u32| {
            if id == 0 {
                (600u64, 1_000)
            } else {
                (4_000u64, 2_000)
            }
        };
        drive_cap(&mut p, 40, mk);

        let ch0 = p.deck().sink().channel(0);
        assert!(ch0.len() > 700, "played through track 0 into track 1");
        // A natural (non-seek) track change never fades — only an explicit seek
        // calls discard.
        assert_eq!(
            p.deck().sink().discards,
            0,
            "a gapless advance must not seek/fade the splice"
        );

        // Track 0 fills frames 0..600, track 1 fills 600.. — one clean transition,
        // both sides non-silent, no gap. (Relative, so independent of the deck's
        // normalization scale.)
        let a = ch0[599];
        let b = ch0[601];
        assert!(a != 0 && b != 0, "neither side of the splice is silence");
        assert!(
            a != b,
            "the track change lands exactly at track 0's 600-frame end"
        );
        assert!(
            ch0[560..600].iter().all(|&s| s == a),
            "track 0 plays steady up to its end"
        );
        assert!(
            ch0[601..651].iter().all(|&s| s == b),
            "track 1 plays steady from its start — contiguous, no gap"
        );
    }

    #[test]
    fn seek_clamps_at_track_bounds() {
        let mut p = cap_player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive_cap(&mut p, 5, |_| (FRAMES, 1_000_000));

        // A negative requested time clamps to the start.
        apply_command(&mut p, Command::Seek { secs: -5.0 }, 0).unwrap();
        assert_eq!(p.position_frames(), 0, "negative seek clamps to frame 0");

        // A time far past the end clamps to the track's total frame count.
        apply_command(&mut p, Command::Seek { secs: 1_000_000.0 }, 0).unwrap();
        assert_eq!(
            p.position_frames(),
            FRAMES,
            "over-seek clamps to the track end, not past it"
        );
    }

    // --- ReplayGain normalization policy + application ----------------------

    #[test]
    fn applied_gain_db_follows_the_mode_policy() {
        let tagged = RgInfo {
            track_gain_db: Some(-6.0),
            album_gain_db: Some(-9.0),
        };
        // Off ignores both the tags and the preamp (unity passthrough).
        assert_eq!(
            applied_gain_db(ReplayGainMode::Off, 3.0, Some(&tagged)),
            0.0
        );
        // Track uses the track gain + preamp.
        assert_eq!(
            applied_gain_db(ReplayGainMode::Track, 3.0, Some(&tagged)),
            -3.0
        );
        // Album uses the album gain + preamp.
        assert_eq!(
            applied_gain_db(ReplayGainMode::Album, 3.0, Some(&tagged)),
            -6.0
        );
        // Album falls back to the track gain when there is no album gain.
        let track_only = RgInfo {
            track_gain_db: Some(-6.0),
            album_gain_db: None,
        };
        assert_eq!(
            applied_gain_db(ReplayGainMode::Album, 0.0, Some(&track_only)),
            -6.0
        );
        // Untagged (None) in Track/Album → just the preamp; Off still 0.
        assert_eq!(applied_gain_db(ReplayGainMode::Track, 4.0, None), 4.0);
        assert_eq!(applied_gain_db(ReplayGainMode::Album, 4.0, None), 4.0);
        assert_eq!(applied_gain_db(ReplayGainMode::Off, 4.0, None), 0.0);
    }

    #[test]
    fn apply_replay_gain_sets_the_deck_gain_without_panicking() {
        // Mirror the backend's pump: play a track, then apply a ReplayGain and
        // confirm the deck actually holds the computed dB (device-free NullSink).
        let mut p = player();
        apply_command(
            &mut p,
            Command::PlayContext {
                ids: vec![0],
                start: 0,
            },
            0,
        )
        .unwrap();
        drive(&mut p, 3);
        let info = RgInfo {
            track_gain_db: Some(-6.0),
            album_gain_db: Some(-9.0),
        };
        let db = apply_replay_gain(&mut p, ReplayGainMode::Track, 3.0, Some(&info));
        assert!((db - (-3.0)).abs() < 1e-6, "track gain -6 + preamp 3 = -3");
        assert!(
            (p.deck().replay_gain_db() - (-3.0)).abs() < 1e-6,
            "the deck holds the applied ReplayGain"
        );
        // Switching Off returns the deck to unity.
        let off = apply_replay_gain(&mut p, ReplayGainMode::Off, 3.0, Some(&info));
        assert_eq!(off, 0.0);
        assert_eq!(p.deck().replay_gain_db(), 0.0, "Off restores unity gain");
    }
}
