//! Audio sink selection for the desktop host: the real `CpalSink` (shared mode),
//! a Windows-only WASAPI **exclusive** (bit-perfect) sink, or a headless
//! `NullSink` for device-free runs and unit tests. Mirrors the sink choice that
//! previously lived in the (now-removed) Tauri head's `actor.rs`/`offline.rs`.
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use soul_audio_core::{SampleFormat, Sink, SinkError, StreamInfo};
use soul_audio_desktop::sink::CpalSink;
#[cfg(windows)]
use soul_audio_desktop::{WasapiExclusiveSink, default_output_config};
use soul_player_backend::{AudioPlayer, Event, Library};

use crate::analyzer::SpectrumShared;
use crate::eq::{DspShared, DspSink};

/// Which sink the actor plays through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkMode {
    /// The real audio device (`CpalSink`, or the exclusive sink when requested).
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

/// What an **exclusive-output** request resolved to on the most recent actor spawn,
/// so the UI can tell the truth: active (and at what device format), fell back to
/// shared (and why), or shared by choice. The live `exclusive` flag on the signal
/// path says whether the *current* stream is exclusive; this adds the *reason* a
/// requested exclusive didn't take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusiveStatus {
    /// Exclusive was not requested — shared mode by choice.
    Shared,
    /// Exclusive is active, at this negotiated device format.
    Active { rate: u32, channels: u16 },
    /// Exclusive was requested but fell back to shared; a human-readable reason
    /// (device in use, no exclusive format, not available on this platform, …).
    FellBack(String),
}

/// The chosen device sink: shared-mode cpal, or (Windows) a WASAPI exclusive-mode
/// stream — `soul-audio-desktop`'s `wasapi-exclusive` feature is always enabled on
/// Windows (see Cargo.toml), so `cfg(windows)` gates the exclusive arm. One concrete
/// type so the pump's
/// `FnOnce` factory returns a single `Sink` impl whichever mode wins — the engine
/// never sees which it got (it reads truth back through [`Sink::exclusive`] /
/// [`Sink::stream_info`]).
enum DeviceSink {
    Shared(CpalSink),
    #[cfg(windows)]
    Exclusive(WasapiExclusiveSink),
}

impl Sink for DeviceSink {
    fn stream_info(&self) -> StreamInfo {
        match self {
            DeviceSink::Shared(s) => s.stream_info(),
            #[cfg(windows)]
            DeviceSink::Exclusive(s) => s.stream_info(),
        }
    }

    fn exclusive(&self) -> bool {
        match self {
            DeviceSink::Shared(s) => s.exclusive(),
            #[cfg(windows)]
            DeviceSink::Exclusive(s) => s.exclusive(),
        }
    }

    fn write(&mut self, frames: &[i32]) -> Result<(), SinkError> {
        match self {
            DeviceSink::Shared(s) => s.write(frames),
            #[cfg(windows)]
            DeviceSink::Exclusive(s) => s.write(frames),
        }
    }

    fn flush(&mut self) -> Result<(), SinkError> {
        match self {
            DeviceSink::Shared(s) => s.flush(),
            #[cfg(windows)]
            DeviceSink::Exclusive(s) => s.flush(),
        }
    }

    fn discard(&mut self) -> Result<(), SinkError> {
        match self {
            DeviceSink::Shared(s) => s.discard(),
            #[cfg(windows)]
            DeviceSink::Exclusive(s) => s.discard(),
        }
    }

    fn set_volume(&mut self, volume: f32) {
        match self {
            DeviceSink::Shared(s) => s.set_volume(volume),
            #[cfg(windows)]
            DeviceSink::Exclusive(s) => s.set_volume(volume),
        }
    }

    fn reopen(&mut self, rate: u32) -> Result<(), SinkError> {
        match self {
            // A shared sink keeps its fixed rate (default no-op) — the deck resamples.
            DeviceSink::Shared(s) => s.reopen(rate),
            // The exclusive sink releases + re-acquires the device at the new rate.
            #[cfg(windows)]
            DeviceSink::Exclusive(s) => s.reopen(rate),
        }
    }
}

/// Spawn the backend actor over the sink `mode` selects. The sink factory runs on
/// the pump thread, so a `!Send` device sink is fine.
///
/// `output_device` names the device the `Device` arm should open (one of
/// [`soul_audio_desktop::output_device_names`]); `None` means the system default.
/// `exclusive` asks for a bit-perfect WASAPI exclusive stream, falling back to
/// shared (recording why in `status`) if the device refuses it. Both are ignored
/// by `Offline` (the `NullSink` has no device to pick).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_actor(
    mode: SinkMode,
    output_device: Option<String>,
    exclusive: bool,
    status: Arc<Mutex<ExclusiveStatus>>,
    dsp: DspShared,
    spectrum: SpectrumShared,
    lib: Arc<Library>,
    on_event: impl Fn(Event) + Send + 'static,
) -> AudioPlayer {
    match mode {
        // Offline/headless has no audible output, so the EQ/analyzer are moot there.
        SinkMode::Offline => AudioPlayer::spawn(|| Ok(NullSink::new(48_000, 2)), lib, on_event),
        SinkMode::Device => AudioPlayer::spawn(
            device_sink_factory(output_device, exclusive, status, dsp, spectrum),
            lib,
            on_event,
        ),
    }
}

/// Build the pump-thread make-sink closure for the real device: try exclusive first
/// when asked (bit-perfect, Windows only, at the device's native rate), else — or on
/// an exclusive failure — open the named shared device (falling back to the system
/// default if it is gone), then wrap it in the [`DspSink`] so the DSP chain runs at
/// the device rate. Called ONCE for the initial sink; per-track native-rate switching
/// is done by the engine reopening this sink in place (`Sink::reopen`), not by
/// rebuilding it, so this is an `FnOnce`.
fn device_sink_factory(
    output_device: Option<String>,
    exclusive: bool,
    status: Arc<Mutex<ExclusiveStatus>>,
    dsp: DspShared,
    spectrum: SpectrumShared,
) -> impl FnOnce() -> anyhow::Result<DspSink<DeviceSink>> + Send + 'static {
    move || {
        let device = open_device_sink(output_device, exclusive, &status)?;
        Ok(DspSink::new(device, dsp, spectrum))
    }
}

/// Open the device sink, honoring `exclusive` when possible and recording the
/// outcome in `status`. Exclusive is attempted only on Windows (via WASAPI), at the
/// device's native rate. On an exclusive failure it falls back to a shared `CpalSink`
/// — never silent, never a hard stop — and records [`ExclusiveStatus::FellBack`]. A
/// successful exclusive open records [`ExclusiveStatus::Active`].
fn open_device_sink(
    output_device: Option<String>,
    exclusive: bool,
    status: &Arc<Mutex<ExclusiveStatus>>,
) -> anyhow::Result<DeviceSink> {
    if exclusive {
        #[cfg(windows)]
        match try_open_exclusive(output_device.clone()) {
            Ok(sink) => {
                let si = sink.stream_info();
                *status.lock().expect("exclusive status mutex") = ExclusiveStatus::Active {
                    rate: si.sample_rate,
                    channels: si.channels,
                };
                return Ok(DeviceSink::Exclusive(sink));
            }
            Err(reason) => {
                *status.lock().expect("exclusive status mutex") = ExclusiveStatus::FellBack(reason);
                // fall through to the shared sink
            }
        }
        #[cfg(not(windows))]
        {
            *status.lock().expect("exclusive status mutex") = ExclusiveStatus::FellBack(
                "exclusive output is not available on this platform".to_string(),
            );
        }
    } else {
        *status.lock().expect("exclusive status mutex") = ExclusiveStatus::Shared;
    }

    // Shared fallback: named device, else the system default (and if the named
    // device is gone since enumeration, the system default).
    let shared = match output_device {
        Some(name) => CpalSink::new_on_device(&name).or_else(|_| CpalSink::new()),
        None => CpalSink::new(),
    }?;
    Ok(DeviceSink::Shared(shared))
}

/// Open the WASAPI exclusive sink at the device's native (shared mix-format) rate —
/// the rate a DAC also accepts exclusively. Per-track native-rate switching then
/// reopens this same sink in place. Returns the failure reason as a string so the
/// caller can record it and fall back to shared.
#[cfg(windows)]
fn try_open_exclusive(name: Option<String>) -> Result<WasapiExclusiveSink, String> {
    let (rate, channels) = default_output_config(name.as_deref())
        .ok_or_else(|| "output device unavailable".to_string())?;
    WasapiExclusiveSink::open(name, rate, channels).map_err(|e| e.to_string())
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
