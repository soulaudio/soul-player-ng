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
/// so the UI can tell the truth: active, fell back to shared (and why), unavailable
/// under strict mode (and why), or shared by choice. The live rate / format comes
/// from the signal path (which tracks per-track native-rate reopens); this says only
/// *whether* exclusive took and *why not* when it didn't — so it never goes stale on
/// a reopen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusiveStatus {
    /// Exclusive was not requested — shared mode by choice.
    Shared,
    /// Exclusive is active (the live format is on the signal path).
    Active,
    /// Exclusive was requested but fell back to shared; a human-readable reason
    /// (device in use, no exclusive format, not available on this platform, …).
    FellBack(String),
    /// Exclusive was requested with **strict** mode on and could not be opened, so
    /// playback did NOT start (never silently downgraded to shared); the reason.
    Unavailable(String),
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
/// `exclusive` asks for a bit-perfect WASAPI exclusive stream; `strict` makes that
/// **exclusive-or-stop** (no fall-back to shared — the actor doesn't start if
/// exclusive can't open). All are ignored by `Offline` (the `NullSink` has no device
/// to pick). The outcome is recorded in `status`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_actor(
    mode: SinkMode,
    output_device: Option<String>,
    exclusive: bool,
    strict: bool,
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
            device_sink_factory(output_device, exclusive, strict, status, dsp, spectrum),
            lib,
            on_event,
        ),
    }
}

/// Build the pump-thread make-sink closure for the real device: try exclusive first
/// when asked (bit-perfect, Windows only, at the device's native rate), else — or on
/// an exclusive failure in non-strict mode — open the named shared device (falling
/// back to the system default if it is gone), then wrap it in the [`DspSink`] so the
/// DSP chain runs at the device rate. Called ONCE for the initial sink; per-track
/// native-rate switching reopens this sink in place (`Sink::reopen`), so it's an
/// `FnOnce`.
fn device_sink_factory(
    output_device: Option<String>,
    exclusive: bool,
    strict: bool,
    status: Arc<Mutex<ExclusiveStatus>>,
    dsp: DspShared,
    spectrum: SpectrumShared,
) -> impl FnOnce() -> anyhow::Result<DspSink<DeviceSink>> + Send + 'static {
    move || {
        let device = open_device_sink(output_device, exclusive, strict, &status)?;
        Ok(DspSink::new(device, dsp, spectrum))
    }
}

/// Open the device sink, honoring `exclusive` when possible and recording the outcome
/// in `status`. Exclusive is attempted only on Windows (via WASAPI), at the device's
/// native rate. On an exclusive failure:
///
/// - non-`strict`: fall back to a shared `CpalSink` — never silent — and record
///   [`ExclusiveStatus::FellBack`];
/// - `strict` (exclusive-or-stop): do NOT fall back — record
///   [`ExclusiveStatus::Unavailable`] and return `Err`, so the actor doesn't start
///   (no silent downgrade to the OS mixer).
///
/// A successful exclusive open records [`ExclusiveStatus::Active`].
fn open_device_sink(
    output_device: Option<String>,
    exclusive: bool,
    strict: bool,
    status: &Arc<Mutex<ExclusiveStatus>>,
) -> anyhow::Result<DeviceSink> {
    if exclusive {
        // The exclusive-open failure reason (set on either platform below), used to
        // decide strict-stop vs shared-fallback once, after the attempt.
        let reason: String;
        #[cfg(windows)]
        {
            match try_open_exclusive(output_device.clone()) {
                Ok(sink) => {
                    *status.lock().expect("exclusive status mutex") = ExclusiveStatus::Active;
                    return Ok(DeviceSink::Exclusive(sink));
                }
                Err(r) => reason = r,
            }
        }
        #[cfg(not(windows))]
        {
            reason = "exclusive output is not available on this platform".to_string();
        }

        if strict {
            // Exclusive-or-stop: don't downgrade to shared — stop here.
            *status.lock().expect("exclusive status mutex") =
                ExclusiveStatus::Unavailable(reason.clone());
            anyhow::bail!("exclusive output unavailable (strict mode): {reason}");
        }
        *status.lock().expect("exclusive status mutex") = ExclusiveStatus::FellBack(reason);
        // fall through to the shared sink
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
