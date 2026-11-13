//! App-facing command, event, and DTO types crossing the UI↔backend boundary.
//! Concrete (u32 track ids, seconds) and serde-serializable — distinct from
//! `soul_audio_control::Command<C>` (generic, frames).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use soul_audio_core::DeckState;
use soul_audio_queue::Repeat;

/// A command from the UI to the player actor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Play an ordered list of track ids, starting at `start`.
    PlayContext {
        ids: Vec<u32>,
        /// Starting index into `ids`; u32 since track counts fit comfortably.
        start: u32,
    },
    TogglePlay,
    Play,
    Pause,
    Stop,
    Next,
    Prev,
    /// Seek within the current track, in seconds from its start.
    Seek {
        secs: f64,
    },
    /// Set output volume, 0.0..=1.0. A positive volume also clears mute.
    SetVolume(f32),
    /// Toggle mute: silence the output while preserving the volume to restore on
    /// un-mute. The player has no separate mute, so this is applied as a 0 gain;
    /// `Tick.muted` reports the state.
    ToggleMute,
    PlayNext(u32),
    Enqueue(u32),
    ClearQueue,
    /// Remove the manually queued track at this index (per-row ✕).
    RemoveFromQueue(u32),
    /// Reorder the manual queue: move the item at `from` to `to` (drag-reorder).
    MoveInQueue {
        from: u32,
        to: u32,
    },
    /// Click the upcoming row at 1-based offset `k`: play it now, consuming the
    /// rows above it (routes to the manual queue or the source, shuffle-correct).
    PlayUpcoming(u32),
    SetShuffle(bool),
    SetRepeat(RepeatMode),
    /// Jump to a natural position within the current context.
    Goto(u32),
    /// Set ReplayGain loudness normalization: the `mode` picks track vs album
    /// (vs off) gain, `preamp_db` is a constant dB offset added on top. The pump
    /// applies the resulting gain to the deck at each track boundary (see
    /// [`ReplayGainMode`]). `Command` is `PartialEq` but not `Eq` precisely so
    /// this `f32` preamp can ride along.
    SetReplayGain {
        mode: ReplayGainMode,
        preamp_db: f32,
    },
    /// Seed the backend's in-memory loudness cache from the app's persisted cache
    /// at startup, so a previously-untagged track analyzed in a past session is
    /// already normalized this session with no re-scan. Each entry is a
    /// `(path, track_gain_db)` pair (the computed gain to the −18 LUFS RG2 target).
    /// The pump inserts them into the cache; see the hybrid ReplayGain path in
    /// `crate::loudness`.
    SeedLoudness(Vec<(PathBuf, f32)>),
}

/// How ReplayGain tags are applied during playback, chosen in Settings →
/// ReplayGain. The per-track dB the pump sets on the deck is:
/// - [`Off`](ReplayGainMode::Off): `0.0` (unity — RG ignored, no preamp).
/// - [`Track`](ReplayGainMode::Track): `track_gain + preamp` (untagged → `preamp`).
/// - [`Album`](ReplayGainMode::Album): `album_gain` (falling back to the track
///   gain), `+ preamp` (untagged → `preamp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReplayGainMode {
    /// No normalization — the deck plays at unity gain.
    #[default]
    Off,
    /// Normalize each track to its own ReplayGain track-gain tag.
    Track,
    /// Normalize to the album-gain tag (preserving inter-track dynamics), so a
    /// whole album plays at one consistent level.
    Album,
}

/// Repeat mode as seen by the UI (mirrors `soul_audio_queue::Repeat`, decoupled
/// from that crate's `serde` feature).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl From<RepeatMode> for Repeat {
    fn from(m: RepeatMode) -> Self {
        match m {
            RepeatMode::Off => Repeat::Off,
            RepeatMode::All => Repeat::All,
            RepeatMode::One => Repeat::One,
        }
    }
}
impl From<Repeat> for RepeatMode {
    fn from(r: Repeat) -> Self {
        match r {
            Repeat::Off => RepeatMode::Off,
            Repeat::All => RepeatMode::All,
            Repeat::One => RepeatMode::One,
        }
    }
}

/// Playback state as seen by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayState {
    Playing,
    Paused,
    Stopped,
}

impl From<DeckState> for PlayState {
    fn from(s: DeckState) -> Self {
        match s {
            DeckState::Playing => PlayState::Playing,
            DeckState::Paused => PlayState::Paused,
            DeckState::Stopped | DeckState::Ended => PlayState::Stopped,
        }
    }
}

/// An event from the player actor to the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Tick(Tick),
    Queue(QueueState),
    /// The current track's decode→output chain (`None` = nothing playing), for the
    /// signal-path display. Emitted alongside `Tick`/`Queue` so it tracks rate /
    /// ReplayGain / device changes.
    SignalPath(Option<SignalPath>),
    /// A previously-untagged track finished background loudness analysis: `path`
    /// identifies the file and `gain_db` is the computed ReplayGain track gain (to
    /// the −18 LUFS RG2 target), emitted once when the analysis completes. The app
    /// persists it (keyed by path + mtime/size) and seeds it back next launch via
    /// [`Command::SeedLoudness`]. No UI is required — a silent background cache is
    /// the intended behavior.
    TrackLoudnessAnalyzed {
        path: PathBuf,
        gain_db: f32,
    },
}

/// Now-playing + transport snapshot, emitted ~8×/s and on any change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tick {
    pub now_playing_id: Option<u32>,
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
    pub state: PlayState,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// The user's volume level, 0.0..=1.0 — preserved across mute (so un-muting
    /// restores it). The actual output gain is 0 while [`Tick::muted`].
    pub volume: f32,
    /// Whether the output is muted (silenced) while keeping `volume`.
    pub muted: bool,
}

/// Up-Next + context position, emitted on queue mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueState {
    /// The manual queue ("Next in queue"): tracks the user explicitly queued via
    /// `PlayNext`/`Enqueue`, in play order. Reorderable/removable.
    pub up_next_ids: Vec<u32>,
    /// The real upcoming CONTEXT tracks ("Next from <album/playlist>") after the
    /// manual queue — bounded, shuffle-correct, projected from the player (not
    /// synthesized from library order). Not reorderable.
    pub context_ids: Vec<u32>,
    /// u32 since context positions fit comfortably.
    pub context_pos: u32,
    pub context_len: u32,
}

/// A library track as sent to the UI (no path, no cover bytes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackView {
    pub id: u32,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_secs: f64,
}

/// The current track's ORIGINAL source format, for the signal-path display.
/// Distinguishes native DSD (which decodes to PCM but should read as "DSD64")
/// from plain PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceFormat {
    /// Multi-bit PCM at this bit depth; its rate is [`SignalPath::decoded_rate`].
    Pcm { bits: u8 },
    /// Native 1-bit DSD at this stream rate in Hz (2_822_400 = DSD64, …).
    Dsd { rate_hz: u32 },
}

/// The current track's real decode → DSP → output chain — what the engine
/// actually does to the samples, for a "signal path" display. A `device_rate`
/// equal to `decoded_rate` with `!resampling` is a sample-rate-exact path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalPath {
    /// The ORIGINAL source (DSD stays DSD even though it decodes to PCM).
    pub source: SourceFormat,
    /// The PCM the decoder hands the engine.
    pub decoded_rate: u32,
    pub decoded_bits: u8,
    pub channels: u8,
    /// Applied ReplayGain, in dB (`0.0` = unity/off). The mode lives in [`Tick`].
    pub replay_gain_db: f32,
    /// Device (sink) sample rate.
    pub device_rate: u32,
    /// Whether the engine resamples `decoded_rate` → `device_rate`.
    pub resampling: bool,
    /// Output bit depth fed to the device (canonical 24-bit today).
    pub output_bits: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_mode_roundtrips_through_queue_repeat() {
        for m in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
            assert_eq!(RepeatMode::from(Repeat::from(m)), m);
        }
    }

    #[test]
    fn command_serde_roundtrips() {
        let cmd = Command::PlayContext {
            ids: vec![3, 1, 2],
            start: 1,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    }

    #[test]
    fn set_replay_gain_command_serde_roundtrips() {
        let cmd = Command::SetReplayGain {
            mode: ReplayGainMode::Album,
            preamp_db: -3.5,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    }

    #[test]
    fn seed_loudness_command_serde_roundtrips() {
        let cmd = Command::SeedLoudness(vec![
            (PathBuf::from("/music/a.flac"), -6.0),
            (PathBuf::from("/music/b.wav"), 2.5),
        ]);
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    }

    #[test]
    fn track_loudness_analyzed_event_serde_roundtrips() {
        let ev = Event::TrackLoudnessAnalyzed {
            path: PathBuf::from("/music/untagged.flac"),
            gain_db: -3.25,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), ev);
    }

    #[test]
    fn deck_state_maps_ended_to_stopped() {
        assert_eq!(PlayState::from(DeckState::Ended), PlayState::Stopped);
        assert_eq!(PlayState::from(DeckState::Playing), PlayState::Playing);
    }
}
