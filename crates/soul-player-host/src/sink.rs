//! Audio sink selection for the desktop host: the real `CpalSink`, or a headless
//! `NullSink` for device-free runs and unit tests. Mirrors the sink choice that
//! previously lived in the (now-removed) Tauri head's `actor.rs`/`offline.rs`.
#![forbid(unsafe_code)]

use std::sync::Arc;

use soul_audio_core::{SampleFormat, Sink, SinkError, StreamInfo};
use soul_audio_desktop::sink::CpalSink;
use soul_player_backend::{AudioPlayer, Event, Library};

/// Which sink the actor plays through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkMode {
    /// The real audio device (`CpalSink`).
    Device,
    /// A discard sink — no device, no backpressure. For `SOUL_PLAYER_OFFLINE`
    /// runs and tests.
    Offline,
}

impl SinkMode {
    /// `Offline` iff `SOUL_PLAYER_OFFLINE` is set, else `Device`.
    pub fn from_env() -> Self {
        if std::env::var_os("SOUL_PLAYER_OFFLINE").is_some() {
            SinkMode::Offline
        } else {
            SinkMode::Device
        }
    }
}

/// Spawn the backend actor over the sink `mode` selects. The sink factory runs on
/// the pump thread, so a `!Send` device sink is fine.
pub(crate) fn build_actor(
    mode: SinkMode,
    lib: Arc<Library>,
    on_event: impl Fn(Event) + Send + 'static,
) -> AudioPlayer {
    match mode {
        SinkMode::Offline => AudioPlayer::spawn(|| Ok(NullSink::new(48_000, 2)), lib, on_event),
        SinkMode::Device => AudioPlayer::spawn(CpalSink::new, lib, on_event),
    }
}

/// A [`Sink`] that discards output — no device, no backpressure.
pub struct NullSink {
    rate: u32,
    channels: u16,
}

impl NullSink {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self { rate, channels }
    }
}

impl Sink for NullSink {
    fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            sample_rate: self.rate,
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }
    fn write(&mut self, _frames: &[i32]) -> Result<(), SinkError> {
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
}
