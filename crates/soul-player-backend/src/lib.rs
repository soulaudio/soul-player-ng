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
pub mod library;
pub mod loudness;
pub mod types;

#[cfg(test)]
pub(crate) mod testutil;

pub use actor::AudioPlayer;
pub use art::{Cover, cover_bytes};
pub use driver::{RgInfo, ServicePlayer, VolumeState, apply_command, project_queue, project_tick};
pub use library::Library;
pub use soul_audio_catalog::BrowseAxis;
pub use types::{
    Command, Event, PlayState, QueueState, RepeatMode, ReplayGainMode, Tick, TrackView,
};
