//! The concrete desktop `Player` specialization plus the pure functions that
//! translate an app [`Command`] into `Player` calls and project the `Player`'s
//! state into UI [`Tick`]/[`QueueState`] events. Generic over the sink and
//! decoder so the whole thing is unit-tested headlessly (NullSink + a const
//! decoder) exactly as it runs with a real device.

use std::collections::VecDeque;

use soul_audio_control::Player;
use soul_audio_core::{DeckError, SeekableDecoder, Sink};
use soul_audio_queue::{SliceContext, TwoLevelQueue};

use crate::types::{Command, PlayState, QueueState, Tick};

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
    }
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
    QueueState {
        up_next_ids: q.upnext_iter().collect(),
        context_ids: q
            .upcoming_iter()
            .skip(manual)
            .take(UPCOMING_CONTEXT_CAP)
            .collect(),
        context_pos: q.pos() as u32,
        context_len: q.source_len() as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{CapturingSink, ConstDecoder, NullSink};
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
        assert_eq!(project_tick(&p, 1.0).now_playing_id, Some(0));
        assert_eq!(project_tick(&p, 1.0).state, PlayState::Playing);
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
        assert_eq!(project_tick(&p, 1.0).state, PlayState::Paused);
        apply_command(&mut p, Command::TogglePlay, 0).unwrap();
        assert_eq!(project_tick(&p, 1.0).state, PlayState::Playing);
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
        assert_eq!(project_tick(&p, 1.0).now_playing_id, Some(11));
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
        assert!(project_tick(&p, 1.0).shuffle);
        apply_command(&mut p, Command::SetRepeat(RepeatMode::All), 0).unwrap();
        assert_eq!(project_tick(&p, 1.0).repeat, RepeatMode::All);
        apply_command(&mut p, Command::SetShuffle(false), 0).unwrap();
        assert!(!project_tick(&p, 1.0).shuffle);
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
        assert_eq!(project_tick(&p, 1.0).state, PlayState::Paused);
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
        assert_eq!(project_tick(&p, 1.0).state, PlayState::Stopped);
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
}
