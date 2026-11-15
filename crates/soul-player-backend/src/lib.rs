//! Headless desktop-player backend service: the command/event/DTO types, a
//! `soul-audio-tags` library scan, the app-command → `soul_audio_control::Player`
//! translation + state projection, and a threaded `AudioPlayer` actor. Owns all
//! backend logic; the UI head (soul-player-host + app/rune) is thin plumbing
//! over this crate.
//!
//! Sink-generic: the actor builds the sink on the pump thread, so a `!Send`
//! device sink works, and this crate stays free of `cpal` (the host injects it).
#![forbid(unsafe_code)]

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
    RgInfo, ServicePlayer, VolumeState, apply_command, project_queue, project_signal_path,
    project_tick,
};
pub use fingerprint::{Fingerprint, fingerprint_file, fingerprint_pcm};
pub use identify::{AcousticId, IdentifyCache};
pub use library::{IncrementalBuild, Library};
pub use soul_audio_catalog::BrowseAxis;
/// The importer's reusable scan cache + its version tag, surfaced so the host can
/// persist it (warm startup) through the one backend contract.
pub use soul_audio_import::{SCAN_CACHE_VERSION, ScanCache, ScanPhase, ScanProgress};
pub use types::{
    Command, Event, PlayState, QueueState, RepeatMode, ReplayGainMode, SignalPath, SourceCodec,
    SourceFormat, Tick, TrackView,
};
