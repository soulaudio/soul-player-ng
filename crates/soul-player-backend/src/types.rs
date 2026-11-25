//! App-facing command, event, and DTO types crossing the UI↔backend boundary.
//! Concrete (u32 track ids, seconds) and serde-serializable, distinct from
//! `soul_audio_control::Command<C>` (generic, frames).

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use soul_audio_core::DeckState;
use soul_audio_import::SkippedGroup;
use soul_audio_loudness::LoudnessMetrics;
use soul_audio_queue::Repeat;

/// A durable reference to a track, and a point-in-time metadata snapshot
/// carried alongside it in a playlist entry: the shared no_std playlist
/// model (`soul-audio-playlist`), re-exported here so every existing
/// importer of `crate::types::{TrackLocator, TrackSnapshot}` keeps resolving
/// to the SAME types the backend, host and app all agree on.
pub use soul_audio_playlist::{TrackLocator, TrackSnapshot};

/// Per-track loudness & dynamic-range metrics for the UI readouts and ReplayGain,
/// the serde-able DTO mirror of the core analyzer's [`LoudnessMetrics`]. All in
/// standard units: `integrated_lufs`/`true_peak_dbtp` absolute, `lra`/`plr` in LU,
/// `dr` the integer DR value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackLoudness {
    /// Integrated (program) loudness in LUFS, negative; −18 is the RG2 reference.
    pub integrated_lufs: f32,
    /// Loudness range in LU: the spread between the quiet and loud passages.
    pub lra: f32,
    /// True-peak level in dBTP (inter-sample peak); ≤ 0 means no clipping.
    pub true_peak_dbtp: f32,
    /// Peak-to-loudness ratio in LU (true-peak minus integrated loudness).
    pub plr: f32,
    /// The integer DR (dynamic-range) value, crest-factor based, not true LRA.
    pub dr: f32,
}

impl From<LoudnessMetrics> for TrackLoudness {
    fn from(m: LoudnessMetrics) -> Self {
        Self {
            integrated_lufs: m.integrated_lufs,
            lra: m.lra,
            true_peak_dbtp: m.true_peak_dbtp,
            plr: m.plr,
            dr: m.dr,
        }
    }
}

// The baked `.soulidx` loudness record ([`soul_audio_catalog::LoudnessRec`]) mirrors these
// fields 1:1, `into()` both ways bridges the DTO and the on-disk record for the bake + readback.
impl From<soul_audio_catalog::LoudnessRec> for TrackLoudness {
    fn from(r: soul_audio_catalog::LoudnessRec) -> Self {
        Self {
            integrated_lufs: r.integrated_lufs,
            lra: r.lra,
            true_peak_dbtp: r.true_peak_dbtp,
            plr: r.plr,
            dr: r.dr,
        }
    }
}

impl From<TrackLoudness> for soul_audio_catalog::LoudnessRec {
    fn from(m: TrackLoudness) -> Self {
        Self {
            integrated_lufs: m.integrated_lufs,
            lra: m.lra,
            true_peak_dbtp: m.true_peak_dbtp,
            plr: m.plr,
            dr: m.dr,
        }
    }
}

/// A command from the UI to the player actor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    /// Play an ordered list of track ids, starting at `start`.
    PlayContext {
        /// The ordered track ids forming the playback context.
        ids: Vec<u32>,
        /// Starting index into `ids`; u32 since track counts fit comfortably.
        start: u32,
    },
    /// Toggle between play and pause.
    TogglePlay,
    /// Begin or resume playback.
    Play,
    /// Pause playback, holding the current position.
    Pause,
    /// Stop playback and reset to the track start.
    Stop,
    /// Skip to the next track.
    Next,
    /// Go to the previous track, or restart the current one if played past the
    /// ~3 s "prev restarts" threshold.
    Prev,
    /// Seek within the current track, in seconds from its start.
    Seek {
        /// Target position in seconds from the track start (clamped to the track bounds).
        secs: f64,
    },
    /// Set output volume, 0.0..=1.0. A positive volume also clears mute.
    SetVolume(f32),
    /// Toggle mute: silence the output while preserving the volume to restore on
    /// un-mute. The player has no separate mute, so this is applied as a 0 gain;
    /// `Tick.muted` reports the state.
    ToggleMute,
    /// Insert a track at the front of the manual queue (it plays next).
    PlayNext(u32),
    /// Append a track to the end of the manual queue.
    Enqueue(u32),
    /// Clear the manual queue, leaving the playing context intact.
    ClearQueue,
    /// Remove the manually queued track at this index (per-row ✕).
    RemoveFromQueue(u32),
    /// Reorder the manual queue: move the item at `from` to `to` (drag-reorder).
    MoveInQueue {
        /// Index of the manual-queue item to move.
        from: u32,
        /// Index to move it to.
        to: u32,
    },
    /// Click the upcoming row at 1-based offset `k`: play it now, consuming the
    /// rows above it (routes to the manual queue or the source, shuffle-correct).
    PlayUpcoming(u32),
    /// Turn shuffle on or off (reshuffles the upcoming context).
    SetShuffle(bool),
    /// Set the repeat mode (Off / All / One).
    SetRepeat(RepeatMode),
    /// Jump to a natural position within the current context.
    Goto(u32),
    /// Set ReplayGain loudness normalization: the `mode` picks track vs album
    /// (vs off) gain, `preamp_db` is a constant dB offset added on top. The pump
    /// applies the resulting gain to the deck at each track boundary (see
    /// [`ReplayGainMode`]). `Command` is `PartialEq` but not `Eq` precisely so
    /// this `f32` preamp can ride along.
    SetReplayGain {
        /// Which gain to apply: track vs album (vs off).
        mode: ReplayGainMode,
        /// A constant dB offset added on top of the chosen gain.
        preamp_db: f32,
    },
    /// Seed the backend's in-memory loudness cache from the app's persisted cache
    /// at startup, so a previously-untagged track analyzed in a past session is
    /// already normalized this session with no re-scan. Each entry is a
    /// `(path, track_gain_db)` pair (the computed gain to the −18 LUFS RG2 target).
    /// The pump inserts them into the cache; see the hybrid ReplayGain path in
    /// `crate::loudness`.
    SeedLoudness(Vec<(PathBuf, TrackLoudness)>),
    /// Enqueue a batch of tracks for background loudness analysis (the scan-time
    /// Tier-2 sweep that populates the library loudness/DR column). Each entry is
    /// `(track_id, path)`; the pump's single-flight cache skips already-analyzed or
    /// in-flight paths, so mass-enqueuing a whole library is idempotent.
    AnalyzeTracks(Vec<(u32, PathBuf)>),
    /// Set how the background loudness sweep runs (Off / Throttled / Full). Off stops the
    /// pump enqueuing new sweep work; Throttled pauses the worker while a track plays (the
    /// dropout guard); Full runs it flat-out. The playing track's lazy on-demand analysis is
    /// unaffected.
    SetLoudnessAnalysisMode(LoudnessAnalysisMode),
}

/// How aggressively the background loudness sweep (the scan-time Tier-2 analysis that fills
/// the library loudness/DR column) runs, chosen in Settings. Mirrors Roon's "Background
/// analysis speed": the forum lesson is that the sweep must yield to active playback so it
/// can never cause audio dropouts, and that a real "Off" matters to the bit-perfect crowd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LoudnessAnalysisMode {
    /// No background sweep at all, zero background CPU while listening. Lazy-on-play
    /// analysis still fills the currently playing track (and its column).
    Off,
    /// Low-priority sweep that YIELDS to active playback (the worker pauses while a track
    /// plays) so it can never cause dropouts. The default.
    #[default]
    Throttled,
    /// Run the sweep flat-out, without yielding to playback, to finish as fast as possible.
    Full,
}

impl LoudnessAnalysisMode {
    /// Whether the background sweep should enqueue new work at all (false only for `Off`).
    #[must_use]
    pub fn sweeps(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether the sweep worker should PAUSE right now, given whether a track is playing:
    /// only `Throttled` yields to playback; `Full` runs regardless; `Off` never sweeps.
    #[must_use]
    pub fn should_pause(self, playing: bool) -> bool {
        matches!(self, Self::Throttled) && playing
    }

    /// The persisted config token for this mode.
    #[must_use]
    pub fn as_config(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Throttled => "throttled",
            Self::Full => "full",
        }
    }

    /// Parse a persisted token back to a mode, defaulting to the playback-yielding
    /// [`Throttled`](Self::Throttled) for anything unknown or missing.
    #[must_use]
    pub fn from_config(s: &str) -> Self {
        match s {
            "off" => Self::Off,
            "full" => Self::Full,
            _ => Self::Throttled,
        }
    }
}

/// How ReplayGain tags are applied during playback, chosen in Settings →
/// ReplayGain. The per-track dB the pump sets on the deck is:
/// - [`Off`](ReplayGainMode::Off): `0.0` (unity, RG ignored, no preamp).
/// - [`Track`](ReplayGainMode::Track): `track_gain + preamp` (untagged → `preamp`).
/// - [`Album`](ReplayGainMode::Album): `album_gain` (falling back to the track
///   gain), `+ preamp` (untagged → `preamp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ReplayGainMode {
    /// No normalization: the deck plays at unity gain.
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
    /// No repeat: playback ends after the last context track.
    #[default]
    Off,
    /// Repeat the whole context, looping back to the start after the last track.
    All,
    /// Repeat the current track indefinitely.
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
    /// Audio is flowing to the output.
    Playing,
    /// Playback is paused, holding its position.
    Paused,
    /// Stopped or ended: no current playback position.
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
    /// A now-playing + transport snapshot (see [`Tick`]), emitted ~8×/s and on change.
    Tick(Tick),
    /// The Up-Next + context snapshot (see [`QueueState`]), emitted on queue mutation.
    Queue(QueueState),
    /// The current track's decode→output chain (`None` = nothing playing), for the
    /// signal-path display. Emitted alongside `Tick`/`Queue` so it tracks rate /
    /// ReplayGain / device changes.
    SignalPath(Option<SignalPath>),
    /// A previously-unmeasured track finished background loudness analysis: `path`
    /// identifies the file and `loudness` is its full metric set (integrated LUFS,
    /// LRA, true-peak, PLR, DR), emitted once when the analysis completes. The app
    /// renders the readouts, derives the ReplayGain gain from the integrated
    /// loudness, and persists the set (keyed by path + mtime/size) to seed back next
    /// launch via [`Command::SeedLoudness`].
    TrackLoudnessAnalyzed {
        /// The analyzed file's path (the key the app persists the metrics under).
        path: PathBuf,
        /// The full measured metric set for that file.
        loudness: TrackLoudness,
    },
    /// A library rescan found tracks that disappeared from an **available** source
    /// since the last scan: `root` is the source they were under and `count` is how
    /// many vanished. Emitted once per affected root so a UI can warn ("N tracks
    /// removed from <folder>"). Only genuine per-file deletions from a reachable
    /// root are reported, an offline / unmounted root is skipped wholesale (never
    /// scanned), so unplugging a drive never mass-reports its tracks as removed.
    TracksRemoved {
        /// The source root the vanished tracks were under.
        root: PathBuf,
        /// How many tracks disappeared from that root since the last scan.
        count: usize,
    },
    /// A library rescan walked past audio files it could not import: `root` is the
    /// source they live under, `groups` counts them per extension with the reason.
    /// Emitted once per affected root (never with an empty `groups`), so a UI can
    /// say "3 files skipped, 2 .aac, 1 .wma" and name the cause.
    ///
    /// This exists because the alternative is silence: a user whose `.aac` rips
    /// never appear in the library cannot distinguish a deliberate limitation
    /// (the decoder is gated off in this build, a different build would play
    /// them) from a scan that is simply broken. The reason is what makes the
    /// notice actionable, so it travels with the count.
    TracksSkipped {
        /// The source root the skipped files live under.
        root: PathBuf,
        /// The skipped files grouped by extension, each with its count and reason.
        groups: Vec<SkippedGroup>,
    },
    /// A track the user asked to play could not be started, so playback produced
    /// silence. `unsupported` is `true` when the format can't be decoded in this
    /// build (e.g. AAC, gated off behind a patent) versus `false` for another
    /// failure (unreadable / corrupt file). Emitted once per failed track-start
    /// command so the UI can say WHY nothing played instead of only logging it. The
    /// track is deliberately not named: a rapid auto-skip through several unplayable
    /// tracks collapses to one standing notice rather than a burst of them.
    PlaybackFailed {
        /// The format can't be decoded in this build (vs. a different failure).
        unsupported: bool,
    },
}

/// Now-playing + transport snapshot, emitted ~8×/s and on any change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tick {
    /// The current track's id, or `None` when nothing is loaded.
    pub now_playing_id: Option<u32>,
    /// The current playback position, in seconds from the track start.
    pub position_secs: f64,
    /// The track's total duration in seconds, or `None` when the container can't report it.
    pub duration_secs: Option<f64>,
    /// The transport state (playing / paused / stopped).
    pub state: PlayState,
    /// Whether shuffle is on.
    pub shuffle: bool,
    /// The active repeat mode.
    pub repeat: RepeatMode,
    /// The user's volume level, 0.0..=1.0, preserved across mute (so un-muting
    /// restores it). The actual output gain is 0 while [`Tick::muted`].
    pub volume: f32,
    /// Whether the output is muted (silenced) while keeping `volume`.
    pub muted: bool,
    /// Background loudness sweep's remaining work (queued + analyzing tracks), for the
    /// "Analyzing N" progress indicator. 0 when the sweep is caught up or Off. The pure
    /// `project_tick` leaves this 0; the pump fills it from its loudness cache.
    pub analysis_pending: u32,
}

/// Up-Next + context position, emitted on queue mutation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueState {
    /// The manual queue ("Next in queue"): tracks the user explicitly queued via
    /// `PlayNext`/`Enqueue`, in play order. Reorderable/removable.
    pub up_next_ids: Vec<u32>,
    /// The real upcoming CONTEXT tracks ("Next from <album/playlist>") after the
    /// manual queue, bounded, shuffle-correct, projected from the player (not
    /// synthesized from library order). Not reorderable.
    pub context_ids: Vec<u32>,
    /// The current track's position within the context (0-based). u32 since context
    /// positions fit comfortably.
    pub context_pos: u32,
    /// The total number of tracks in the context.
    pub context_len: u32,
}

/// A library track as sent to the UI (no path, no cover bytes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackView {
    /// The track's (possibly compound, source-tagged) id.
    pub id: u32,
    /// Track title, or `None` when the file carries no title tag. Kept an owned
    /// `String` (not interned): titles are ~unique per track, so there's no
    /// sharing to gain, unlike the low-cardinality `artist`/`album`/`genre`/`format`.
    pub title: Option<String>,
    /// Track artist, or `None` when untagged. Interned as `Arc<str>` so the many
    /// tracks sharing one artist share one allocation, `Library::views_by`
    /// materializes the distinct-artist count of strings, not one-per-track.
    pub artist: Option<Arc<str>>,
    /// Album title, or `None` when untagged. Interned as `Arc<str>` (see `artist`).
    pub album: Option<Arc<str>>,
    /// The track's genre bucket, interned from file tags. Unlike `artist` /
    /// `album`, the importer's `"Unknown Genre"` sentinel is **kept** (not mapped
    /// back to `None`): every track has a genre bucket, and the blank/absent case
    /// is a real, browse-able "Unknown Genre" group that keys the Genre axis.
    /// Interned as `Arc<str>` (see `artist`): genre cardinality is tiny.
    pub genre: Option<Arc<str>>,
    /// Track duration in seconds, tag-derived (0.0 when unknown).
    pub duration_secs: f64,
    /// `None` when this track sits on local disk; `Some(host)` when it
    /// streams from a remote source, naming the remote server (a
    /// human-readable host/label, e.g. `listen.home.lan:8080`) so the UI can
    /// show WHICH server a track streams from (not just that it's remote).
    /// The backend/local catalog projection always sets this `None`; the
    /// host's remote-aware aggregate (`soul-player-host`) sets `Some(host)`
    /// for a track whose compound id falls in a remote server's block, see
    /// `soul_player_host::PlayerHandle::views`.
    pub remote_source: Option<String>,
    /// The album's ORIGINAL-release date, packed `(year<<9)|(month<<5)|day`
    /// (`0` = unknown), mirrors `soul_audio_catalog::PackedDate`. The audiophile
    /// canonical year; the UI prefers it, falling back to `release_date`.
    pub original_date: u32,
    /// The album's main release/reissue date, same packing. Unknown = `0`.
    pub release_date: u32,
    /// Per-track loudness/DR, `None` until analyzed. Sourced from the baked record
    /// and refreshed from the host's live analysis map (`mark_loudness`); drives the
    /// library loudness/DR column and its sort.
    pub loudness: Option<TrackLoudness>,
    /// An uppercased codec/container label derived from the file name's extension
    /// (`"FLAC"`, `"MP3"`, `"WAV"`, `"M4A"`, …), for the Tracks table's color-coded
    /// FORMAT badge. `None` when the name can't resolve or carries no extension.
    /// Cheap: the backend projection (`Library::views_by`) derives this from the
    /// already-resolved file name, no extra decode or path join. Interned as
    /// `Arc<str>`: only a handful of distinct extensions exist across a library,
    /// so one allocation per format is shared by every track that uses it.
    pub format: Option<Arc<str>>,
}

/// The current track's ORIGINAL source format, for the signal-path display.
/// Distinguishes native DSD (which decodes to PCM but should read as "DSD64")
/// from plain PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceFormat {
    /// Multi-bit PCM at this bit depth; its rate is [`SignalPath::decoded_rate`].
    Pcm {
        /// Bit depth of the source PCM samples.
        bits: u8,
    },
    /// Native 1-bit DSD at this stream rate in Hz (2_822_400 = DSD64, …).
    Dsd {
        /// The DSD stream rate in Hz (e.g. 2_822_400 for DSD64).
        rate_hz: u32,
    },
}

/// The current track's original **codec**, the lossless-vs-lossy identity the
/// Source stage names (FLAC / ALAC / AAC / …). [`SourceFormat`] only says PCM vs
/// DSD and the bit depth, so a FLAC, a WAV and an ALAC otherwise read the same.
/// A serde DTO mirroring `soul_audio_core::SourceCodec`, which (being `no_std`)
/// carries no serde of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceCodec {
    /// PCM whose decoder didn't name a more specific codec.
    Pcm,
    /// RIFF/WAVE PCM.
    Wav,
    /// AIFF / AIFF-C PCM.
    Aiff,
    /// FLAC (native or Ogg). Lossless.
    Flac,
    /// Apple Lossless (ALAC), in MP4/M4A or CAF. Lossless.
    Alac,
    /// AAC-LC, in MP4/M4A or ADTS. Lossy.
    Aac,
    /// MP3 (MPEG-1/2 Layer III). Lossy.
    Mp3,
    /// Opus (Ogg or Matroska). Lossy.
    Opus,
    /// Vorbis (Ogg or Matroska). Lossy.
    Vorbis,
    /// WavPack. Lossless.
    WavPack,
    /// Apple CAF container (lpcm/alac not yet split out).
    Caf,
    /// Matroska / WebM audio (FLAC/Opus not yet split out).
    Matroska,
    /// Native 1-bit DSD (DSF / DFF).
    Dsd,
}

impl From<soul_audio_core::SourceCodec> for SourceCodec {
    fn from(c: soul_audio_core::SourceCodec) -> Self {
        use soul_audio_core::SourceCodec as C;
        match c {
            C::Pcm => SourceCodec::Pcm,
            C::Wav => SourceCodec::Wav,
            C::Aiff => SourceCodec::Aiff,
            C::Flac => SourceCodec::Flac,
            C::Alac => SourceCodec::Alac,
            C::Aac => SourceCodec::Aac,
            C::Mp3 => SourceCodec::Mp3,
            C::Opus => SourceCodec::Opus,
            C::Vorbis => SourceCodec::Vorbis,
            C::WavPack => SourceCodec::WavPack,
            C::Caf => SourceCodec::Caf,
            C::Matroska => SourceCodec::Matroska,
            C::Dsd => SourceCodec::Dsd,
            // `soul_audio_core::SourceCodec` is `#[non_exhaustive]`; a codec added
            // there but not yet mirrored here reads as the generic PCM fallback.
            _ => SourceCodec::Pcm,
        }
    }
}

/// The current track's real decode → DSP → output chain, what the engine
/// actually does to the samples, for a "signal path" display. A `device_rate`
/// equal to `decoded_rate` with `!resampling` is a sample-rate-exact path.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalPath {
    /// The ORIGINAL source (DSD stays DSD even though it decodes to PCM).
    pub source: SourceFormat,
    /// The original codec the track is encoded with (FLAC / ALAC / AAC / DSD / …),
    /// the lossless-vs-lossy identity the Source stage names.
    pub codec: SourceCodec,
    /// The PCM the decoder hands the engine.
    pub decoded_rate: u32,
    /// Bit depth of the decoded PCM.
    pub decoded_bits: u8,
    /// Channel count of the decoded PCM.
    pub channels: u8,
    /// Applied ReplayGain, in dB (`0.0` = unity/off). The mode lives in [`Tick`].
    pub replay_gain_db: f32,
    /// Device (sink) sample rate.
    pub device_rate: u32,
    /// Whether the engine resamples `decoded_rate` → `device_rate`.
    pub resampling: bool,
    /// Output bit depth the device actually receives, the shared mixer's format in
    /// shared mode, or the negotiated bit-perfect depth in exclusive mode (no longer
    /// a hardcoded canonical 24).
    pub output_bits: u8,
    /// Whether the sink drives the device in **exclusive** (bit-perfect) mode,
    /// bypassing the OS mixer. `false` for shared mode (the default / the fallback).
    pub exclusive: bool,
    /// The current track's loudness & dynamic-range metrics, once the background
    /// analysis has them cached (`None` while unmeasured, the readouts show
    /// "analyzing…"). Filled by the actor from its loudness cache at projection.
    pub loudness: Option<TrackLoudness>,
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
    fn loudness_analysis_mode_gates_sweep_and_playback_yield() {
        use LoudnessAnalysisMode::{Full, Off, Throttled};
        // Off never sweeps; Throttled and Full both do.
        assert!(!Off.sweeps(), "Off runs no background sweep");
        assert!(Throttled.sweeps());
        assert!(Full.sweeps());
        // Only Throttled yields to active playback.
        assert!(
            Throttled.should_pause(true),
            "Throttled pauses while playing"
        );
        assert!(!Throttled.should_pause(false), "Throttled runs when idle");
        assert!(
            !Full.should_pause(true),
            "Full runs flat-out, even while playing"
        );
        assert!(!Off.should_pause(true), "Off has nothing to pause");
        // Default is the playback-yielding Throttled.
        assert_eq!(LoudnessAnalysisMode::default(), Throttled);
        // Config token roundtrips; an unknown/missing token falls back to Throttled.
        for m in [Off, Throttled, Full] {
            assert_eq!(LoudnessAnalysisMode::from_config(m.as_config()), m);
        }
        assert_eq!(LoudnessAnalysisMode::from_config("bogus"), Throttled);
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

    fn loudness(integrated: f32) -> TrackLoudness {
        TrackLoudness {
            integrated_lufs: integrated,
            lra: 6.0,
            true_peak_dbtp: -1.2,
            plr: 12.0,
            dr: 11.0,
        }
    }

    #[test]
    fn seed_loudness_command_serde_roundtrips() {
        let cmd = Command::SeedLoudness(vec![
            (PathBuf::from("/music/a.flac"), loudness(-12.0)),
            (PathBuf::from("/music/b.wav"), loudness(-20.5)),
        ]);
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    }

    #[test]
    fn analyze_tracks_command_serde_roundtrips() {
        let cmd = Command::AnalyzeTracks(vec![
            (0, PathBuf::from("/music/a.flac")),
            (7, PathBuf::from("/music/b.wav")),
        ]);
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), cmd);
    }

    #[test]
    fn loudness_rec_and_track_loudness_convert_both_ways() {
        let m = loudness(-14.0);
        let rec: soul_audio_catalog::LoudnessRec = m.into();
        let back: TrackLoudness = rec.into();
        assert_eq!(back, m);
    }

    #[test]
    fn track_loudness_analyzed_event_serde_roundtrips() {
        let ev = Event::TrackLoudnessAnalyzed {
            path: PathBuf::from("/music/untagged.flac"),
            loudness: loudness(-14.0),
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
