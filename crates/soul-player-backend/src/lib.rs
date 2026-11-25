//! Headless desktop-player backend service: the command/event/DTO types, a
//! `soul-audio-tags` library scan, the app-command → `soul_audio_control::Player`
//! translation + state projection, and a threaded `AudioPlayer` actor. Owns all
//! backend logic; the UI head (soul-player-host + app) is thin plumbing
//! over this crate.
//!
//! Sink-generic: the actor builds the sink on the pump thread, so a `!Send`
//! device sink works, and this crate stays free of `cpal` (the host injects it).
#![forbid(unsafe_code)]
// Documentation is a CI gate, not just a culture: every public item on this
// cross-layer IPC surface (commands, events, DTOs, the library/actor API) must
// carry a doc comment, so a new undocumented `pub` item fails the build.
#![deny(missing_docs)]

// Licensing guard: AAC is a Via LA patent pool and a clean-room decoder doesn't clear it, so
// soul-audio-core keeps AAC decode behind an opt-in `aac` feature that shipped soul products
// (this player included) must leave OFF. Cargo unifies features across the whole graph, so
// this fails the BUILD if anything in it turns AAC on.
const _: () = assert!(
    !soul_audio_core::AAC_ENABLED,
    "soul-audio-core's `aac` feature is on: shipped soul products must not decode AAC (patent \
     licensing). Remove `features = [\"aac\"]` wherever it was added."
);

pub mod actor;
pub mod art;
pub mod driver;
pub mod fingerprint;
pub mod identify;
pub mod library;
pub mod loudness;
pub mod types;

#[cfg(test)]
pub(crate) mod testutil;

pub use actor::AudioPlayer;
pub use art::{Cover, cover_bytes};
pub use driver::{
    OutputConfig, RgInfo, ServicePlayer, VolumeState, apply_command, project_queue,
    project_signal_path, project_tick,
};
pub use fingerprint::{Fingerprint, fingerprint_file, fingerprint_pcm};
pub use identify::{AcousticId, IdentifyCache};
pub use library::{IncrementalBuild, IndexBlob, Library};
pub use soul_audio_catalog::BrowseAxis;
/// The importer's reusable scan cache + its version tag, surfaced so the host can
/// persist it (warm startup) through the one backend contract.
pub use soul_audio_import::{
    SCAN_CACHE_VERSION, ScanCache, ScanPhase, ScanProgress, SkipReason, SkippedGroup,
};
pub use soul_audio_loudness::LoudnessMetrics;
pub use types::{
    Command, Event, LoudnessAnalysisMode, PlayState, QueueState, RepeatMode, ReplayGainMode,
    SignalPath, SourceCodec, SourceFormat, Tick, TrackLoudness, TrackView,
};
