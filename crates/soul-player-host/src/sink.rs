//! Audio sink selection for the desktop host: the real `CpalSink` (shared mode),
//! an **exclusive** (bit-perfect) sink, WASAPI exclusive on Windows, CoreAudio hog
//! mode with native-rate switching on macOS, the card claimed directly as `hw:CARD,DEV`
//! on Linux, or a headless
//! `NullSink` for device-free runs and unit tests. Mirrors the sink choice that
//! previously lived in the (now-removed) Tauri head's `actor.rs`/`offline.rs`.
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use soul_audio_core::{SampleFormat, Sink, SinkError, StreamInfo};
/// The platform's exclusive (bit-perfect) device sink. All three share one contract:
/// `open(name, rate, channels)`, `exclusive() == true`, and an in-place `reopen(rate)`
/// for per-track native-rate switching, so everything below is platform-neutral past
/// this alias, and which backend it is never escapes this module (the engine reads
/// truth back through `Sink::exclusive`).
#[cfg(target_os = "linux")]
use soul_audio_desktop::AlsaExclusiveSink as ExclusiveSink;
#[cfg(target_os = "macos")]
use soul_audio_desktop::CoreAudioExclusiveSink as ExclusiveSink;
#[cfg(windows)]
use soul_audio_desktop::WasapiExclusiveSink as ExclusiveSink;
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
use soul_audio_desktop::default_output_config;
use soul_audio_desktop::sink::CpalSink;
use soul_player_backend::library::TrackSource;
use soul_player_backend::{AudioPlayer, Event, OutputConfig};

use crate::analyzer::{LiveLoudnessShared, SpectrumShared};
use crate::eq::{DspShared, DspSink};

/// Which sink the actor plays through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkMode {
    /// The real audio device (`CpalSink`, or the exclusive sink when requested).
    Device,
    /// A discard sink: no device, no backpressure. For `SOUL_PLAYER_OFFLINE`
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
/// *whether* exclusive took and *why not* when it didn't, so it never goes stale on
/// a reopen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusiveStatus {
    /// Exclusive was not requested, shared mode by choice.
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

/// The chosen device sink: shared-mode cpal, or an exclusive-mode stream, WASAPI on
/// Windows, CoreAudio (hog mode) on macOS, ALSA `hw:` (claimed over the D-Bus
/// device-reservation protocol) on Linux. `soul-audio-desktop`'s `wasapi-exclusive` /
/// `coreaudio-exclusive` / `alsa-exclusive` feature is always enabled on that OS (see
/// Cargo.toml), so the `cfg(any(..))` below gates the exclusive arm. One concrete
/// type so the pump's
/// `Fn` factory returns a single `Sink` impl whichever mode wins, the engine
/// never sees which it got (it reads truth back through [`Sink::exclusive`] /
/// [`Sink::stream_info`]).
enum DeviceSink {
    Shared(CpalSink),
    #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
    Exclusive(ExclusiveSink),
    /// A device-FREE placeholder: owns no hardware and discards writes. Used to PARK
    /// the deck during a live output change, swapping this in drops the real sink,
    /// which releases its device, so the new sink can acquire it (exclusive WASAPI is
    /// one-client-per-device). The pump swaps it out in the same step, so it's never
    /// actually written to; it reports a sane rate/channels only so `Deck::swap_sink`
    /// can rebuild the resampler without failing.
    Silent {
        rate: u32,
        channels: u16,
    },
}

impl DeviceSink {
    /// A standard-rate device-free placeholder for parking (see [`DeviceSink::Silent`]).
    fn silent() -> Self {
        DeviceSink::Silent {
            rate: 48_000,
            channels: 2,
        }
    }
}

impl Sink for DeviceSink {
    fn stream_info(&self) -> StreamInfo {
        match self {
            DeviceSink::Shared(s) => s.stream_info(),
            #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
            DeviceSink::Exclusive(s) => s.stream_info(),
            DeviceSink::Silent { rate, channels } => StreamInfo {
                sample_rate: *rate,
                channels: *channels,
                format: SampleFormat::Int { bits: 24 },
            },
        }
    }

    fn exclusive(&self) -> bool {
        match self {
            DeviceSink::Shared(s) => s.exclusive(),
            #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
            DeviceSink::Exclusive(s) => s.exclusive(),
            DeviceSink::Silent { .. } => false,
        }
    }

    fn write(&mut self, frames: &[i32]) -> Result<(), SinkError> {
        match self {
            DeviceSink::Shared(s) => s.write(frames),
            #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
            DeviceSink::Exclusive(s) => s.write(frames),
            // Device-free placeholder: discard (never actually reached, the pump
            // swaps it out before pumping).
            DeviceSink::Silent { .. } => Ok(()),
        }
    }

    fn flush(&mut self) -> Result<(), SinkError> {
        match self {
            DeviceSink::Shared(s) => s.flush(),
            #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
            DeviceSink::Exclusive(s) => s.flush(),
            DeviceSink::Silent { .. } => Ok(()),
        }
    }

    fn discard(&mut self) -> Result<(), SinkError> {
        match self {
            DeviceSink::Shared(s) => s.discard(),
            #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
            DeviceSink::Exclusive(s) => s.discard(),
            DeviceSink::Silent { .. } => Ok(()),
        }
    }

    fn set_volume(&mut self, volume: f32) {
        match self {
            DeviceSink::Shared(s) => s.set_volume(volume),
            #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
            DeviceSink::Exclusive(s) => s.set_volume(volume),
            DeviceSink::Silent { .. } => {}
        }
    }

    fn reopen(&mut self, rate: u32) -> Result<(), SinkError> {
        match self {
            // A shared sink keeps its fixed rate (default no-op), the deck resamples.
            DeviceSink::Shared(s) => s.reopen(rate),
            // The exclusive sink releases + re-acquires the device at the new rate.
            #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
            DeviceSink::Exclusive(s) => s.reopen(rate),
            // Placeholder adopts the requested rate (no device to reopen).
            DeviceSink::Silent { rate: r, .. } => {
                *r = rate;
                Ok(())
            }
        }
    }
}

/// Spawn the backend actor over the sink `mode` selects. The sink factory runs on
/// the pump thread, so a `!Send` device sink is fine.
///
/// `output_config` is the host's shared, live-mutable desired output (device /
/// exclusive / strict): the `Device` factory reads it to open the sink, and the pump
/// re-reads it every block so a device/exclusive change applies to the current track
/// (it's ignored by `Offline`, whose `NullSink` has no device). The exclusive outcome
/// is recorded in `status`.
///
/// `lib` is a trait object (not a concrete `Library`) so a remote-aware
/// aggregate ([`crate::remote::RemoteLibrary`]) can be spawned with it too,
/// the same reason [`AudioPlayer::spawn`]'s own parameter is `Arc<dyn
/// TrackSource>`.
#[allow(clippy::too_many_arguments)] // cohesive actor wiring: sink mode + the shared handles it threads
pub(crate) fn build_actor(
    mode: SinkMode,
    output_config: Arc<Mutex<OutputConfig>>,
    status: Arc<Mutex<ExclusiveStatus>>,
    dsp: DspShared,
    spectrum: SpectrumShared,
    live: LiveLoudnessShared,
    lib: Arc<dyn TrackSource>,
    on_event: impl Fn(Event) + Send + 'static,
) -> AudioPlayer {
    match mode {
        // Offline/headless has no audible output, so the EQ/analyzer/meter are moot there.
        SinkMode::Offline => AudioPlayer::spawn(
            |_cfg| Ok(NullSink::new(48_000, 2)),
            || NullSink::new(48_000, 2),
            output_config,
            lib,
            on_event,
        ),
        SinkMode::Device => AudioPlayer::spawn(
            device_sink_factory(status, dsp.clone(), spectrum.clone(), live.clone()),
            device_park_factory(dsp, spectrum, live),
            output_config,
            lib,
            on_event,
        ),
    }
}

/// Build the pump-thread PARK closure: a device-FREE [`DeviceSink::Silent`] wrapped in
/// a [`DspSink`] (same concrete `K` as [`device_sink_factory`] produces). The pump
/// swaps this in to drop the real sink, releasing its device, before opening the new
/// one on a live output change. It owns no hardware, so it never contends for the
/// device. `dsp` / `spectrum` are cloned into it only to match the sink type; the
/// placeholder is swapped out before any audio flows.
fn device_park_factory(
    dsp: DspShared,
    spectrum: SpectrumShared,
    live: LiveLoudnessShared,
) -> impl Fn() -> DspSink<DeviceSink> + Send + 'static {
    move || {
        DspSink::new(
            DeviceSink::silent(),
            dsp.clone(),
            spectrum.clone(),
            live.clone(),
        )
    }
}

/// Build the pump-thread make-sink closure for the real device: for the given
/// [`OutputConfig`], try exclusive first when asked (bit-perfect, Windows only, at the
/// device's native rate), else, or on an exclusive failure in non-strict mode, open
/// the named shared device (falling back to the system default if it is gone), then
/// wrap it in the [`DspSink`] so the DSP chain runs at the device rate. It's an `Fn`,
/// not `FnOnce`: the pump calls it for the initial sink AND again on each live
/// device/exclusive change (per-track native-rate switching still reopens the sink in
/// place via `Sink::reopen`). `dsp` / `spectrum` are `Arc`s cloned into each sink so
/// the one shared DSP chain + analyzer survive a rebuild.
fn device_sink_factory(
    status: Arc<Mutex<ExclusiveStatus>>,
    dsp: DspShared,
    spectrum: SpectrumShared,
    live: LiveLoudnessShared,
) -> impl Fn(&OutputConfig) -> anyhow::Result<DspSink<DeviceSink>> + Send + 'static {
    move |cfg: &OutputConfig| {
        let device = open_device_sink(cfg.device.clone(), cfg.exclusive, cfg.strict, &status)?;
        // Event log (once per sink build): what the device sink ACTUALLY opened as, the
        // requested vs resolved exclusive mode, rate, channels, format, and the exclusive
        // outcome. Makes the output pipeline visible in the logs without per-block noise.
        let si = device.stream_info();
        tracing::info!(
            requested_exclusive = cfg.exclusive,
            requested_device = ?cfg.device,
            got_exclusive = device.exclusive(),
            rate = si.sample_rate,
            channels = si.channels,
            format = ?si.format,
            status = ?status.lock().ok().map(|s| format!("{s:?}")),
            "device sink opened"
        );
        Ok(DspSink::new(
            device,
            dsp.clone(),
            spectrum.clone(),
            live.clone(),
        ))
    }
}

/// Open the device sink, honoring `exclusive` when possible and recording the outcome
/// in `status`. Exclusive is attempted on Windows (WASAPI) and macOS (CoreAudio hog
/// mode), at the device's current rate, per-track native-rate switching then reopens
/// it in place. On an exclusive failure:
///
/// - non-`strict`: fall back to a shared `CpalSink`, never silent, and record
///   [`ExclusiveStatus::FellBack`];
/// - `strict` (exclusive-or-stop): do NOT fall back, record
///   [`ExclusiveStatus::Unavailable`] and return `Err`, so the actor doesn't start
///   (no silent downgrade to the OS mixer).
///
/// A successful exclusive open records [`ExclusiveStatus::Active`].
///
/// Whether this build has a bit-perfect **exclusive** output backend at all.
///
/// A capability, not a preference: where this is false, asking for exclusive
/// can only ever fall back to shared (or, under strict, refuse to play). A UI
/// should say so up front rather than offer a switch that cannot do anything,
/// `strict` especially, where the honest outcome is silence.
///
/// True on all three desktop platforms: WASAPI exclusive on Windows, CoreAudio
/// hog mode on macOS, and direct `hw:CARD,DEV` on Linux. The Linux one is not
/// just "open `hw:X,Y`", a stock desktop has PipeWire/WirePlumber holding every
/// card, so the device is first claimed through the D-Bus device-reservation
/// protocol (`org.freedesktop.ReserveDevice1`, device name `Audio<N>`, held by
/// WirePlumber at priority −20) before ALSA will hand over the hardware. All
/// three sinks live in `soul-audio-core`; this is only the capability flag.
pub const EXCLUSIVE_SUPPORTED: bool =
    cfg!(windows) || cfg!(target_os = "linux") || cfg!(target_os = "macos");

/// The reason reported on a platform with no bit-perfect backend compiled in,
/// everything that is not Windows, macOS or Linux (a BSD, say). Asking for
/// exclusive there can only fall back to shared, so the message names the
/// platform rather than implying a device problem.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
const NO_EXCLUSIVE_BACKEND: &str = "exclusive output is not available on this platform";

/// Record an exclusive-open failure and decide whether playback may continue:
/// `Ok(())` to fall through to the shared sink ([`ExclusiveStatus::FellBack`]),
/// or `Err` under `strict` ("exclusive or stop", [`ExclusiveStatus::Unavailable`])
/// so the actor never silently downgrades to the OS mixer.
///
/// Split out of [`open_device_sink`] so this decision, the part with the
/// user-visible consequence, is unit-testable WITHOUT opening an audio device.
/// Testing it through `open_device_sink` instead would drag in a real device
/// open on the non-strict path, which is slow, needs hardware CI runners don't
/// have, and (as it turned out) stalls the rest of the suite.
fn record_exclusive_failure(
    reason: String,
    strict: bool,
    status: &Arc<Mutex<ExclusiveStatus>>,
) -> anyhow::Result<()> {
    if strict {
        *status.lock().expect("exclusive status mutex") =
            ExclusiveStatus::Unavailable(reason.clone());
        anyhow::bail!("exclusive output unavailable (strict mode): {reason}");
    }
    *status.lock().expect("exclusive status mutex") = ExclusiveStatus::FellBack(reason);
    Ok(())
}

fn open_device_sink(
    output_device: Option<String>,
    exclusive: bool,
    strict: bool,
    status: &Arc<Mutex<ExclusiveStatus>>,
) -> anyhow::Result<DeviceSink> {
    if exclusive {
        // The exclusive-open failure reason (set on whichever arm below compiles),
        // used to decide strict-stop vs shared-fallback once, after the attempt.
        // Exactly one of the two arms exists in any build, `reason` is assigned
        // once, and the `cfg` sets are complements, so it is never left unset.
        let reason: String;
        // One body for all three backends; `try_open_exclusive` is what differs per
        // platform, behind the same signature.
        #[cfg(any(windows, target_os = "macos", target_os = "linux"))]
        {
            match try_open_exclusive(output_device.clone()) {
                Ok(sink) => {
                    *status.lock().expect("exclusive status mutex") = ExclusiveStatus::Active;
                    return Ok(DeviceSink::Exclusive(sink));
                }
                Err(r) => reason = r,
            }
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            reason = NO_EXCLUSIVE_BACKEND.to_string();
        }

        // Strict stops here; non-strict records why and falls through to shared.
        record_exclusive_failure(reason, strict, status)?;
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

/// Open the platform exclusive sink at the device's native (shared mix-format) rate,
/// the rate a DAC also accepts exclusively. Per-track native-rate switching then
/// reopens this same sink in place. Returns the failure reason as a string so the
/// caller can record it and fall back to shared.
///
/// One body for all three backends: the starting rate is discovered the same way
/// (cpal's view of the device), and each sink's own `open` negotiates from there,
/// WASAPI against the endpoint's exclusive formats, CoreAudio against the device's
/// physical formats, ALSA against the card's `hw_params` after claiming it over
/// D-Bus.
#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn try_open_exclusive(name: Option<String>) -> Result<ExclusiveSink, String> {
    // On macOS cpal can't read the config of a device ANOTHER process hogs, which would
    // surface as a vague "unavailable". Fall back to the HAL's own nominal rate (stereo)
    // and let the sink's open, which checks hog ownership first, report the precise
    // reason (device in use / not found).
    #[cfg(target_os = "macos")]
    let config = default_output_config(name.as_deref())
        .or_else(|| soul_audio_desktop::nominal_rate(name.as_deref()).map(|r| (r, 2)));
    // Windows and Linux have no equivalent HAL rate query to fall back on, and cpal can
    // read the shared device's config on both even when the exclusive open is about to
    // claim the endpoint/card directly.
    #[cfg(any(windows, target_os = "linux"))]
    let config = default_output_config(name.as_deref());
    let (rate, channels) = config.ok_or_else(|| "output device unavailable".to_string())?;
    ExclusiveSink::open(name, rate, channels).map_err(|e| e.to_string())
}

/// A [`Sink`] that discards output, no device, no backpressure.
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

#[cfg(test)]
mod device_tests {
    use super::*;

    /// Every platform with an exclusive backend must advertise it, and every
    /// platform without one must not. This is the switch the Settings card reads
    /// to decide whether the toggle is even offered, so a backend that exists but
    /// isn't advertised is invisible to the user, and one advertised but missing
    /// is a control that silently does nothing.
    #[test]
    fn exclusive_is_advertised_exactly_where_a_backend_exists() {
        // Compared against a locally computed expectation rather than asserted
        // directly: `assert!(CONST)` is a clippy error, and this reads as the
        // contract anyway, these two lists must not drift apart.
        let has_backend = cfg!(windows) || cfg!(target_os = "linux") || cfg!(target_os = "macos");
        assert_eq!(
            EXCLUSIVE_SUPPORTED, has_backend,
            "a platform with a backend (WASAPI / ALSA / CoreAudio) must advertise \n             exclusive, and \
             one without must not, otherwise the UI offers a control that does \
             nothing, or hides one that works"
        );
    }

    /// REAL-DEVICE: asking for exclusive output on Linux must actually take the
    /// card bit-perfect, not fall back to the shared mixer.
    ///
    /// The whole feature is only observable against hardware, a sink that opened
    /// shared reports a perfectly healthy stream, so nothing short of asking it
    /// whether it is exclusive catches the regression. Ignored: it takes the sound
    /// card over for a moment.
    ///   `cargo test -p soul-player-host exclusive_open_takes -- --ignored --nocapture`
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "takes over the real sound card; run with --ignored"]
    fn exclusive_open_takes_the_card_bit_perfect() {
        let status = Arc::new(Mutex::new(ExclusiveStatus::Shared));
        let sink = match open_device_sink(None, true, true, &status) {
            Ok(s) => s,
            Err(e) => {
                println!("no usable output device ({e:#}), skipping");
                return;
            }
        };
        let si = sink.stream_info();
        println!(
            "exclusive: {} Hz, {} ch, {:?}",
            si.sample_rate, si.channels, si.format
        );
        assert!(
            sink.exclusive(),
            "strict exclusive returned a sink that is NOT exclusive, it fell back \
             to the shared mixer while reporting success"
        );
        assert_eq!(
            *status.lock().expect("exclusive status mutex"),
            ExclusiveStatus::Active,
            "a successful exclusive open must record Active for the UI"
        );
        assert!(si.sample_rate > 0 && si.channels > 0);
    }

    /// Exclusive output is a Windows (WASAPI) feature today. Everywhere else the
    /// request must be REFUSED visibly rather than quietly pretended, the
    /// [`ExclusiveStatus`] these set is the only thing the Settings card and the
    /// signal-path row read to tell the user whether the stream is bit-perfect.
    ///
    /// Strict mode is the branch with teeth: "exclusive or stop" must never
    /// downgrade to the system mixer behind the user's back.
    #[test]
    fn strict_exclusive_refuses_rather_than_downgrading() {
        let status = Arc::new(Mutex::new(ExclusiveStatus::Shared));
        let outcome = record_exclusive_failure("device in use".into(), true, &status);
        assert!(
            outcome.is_err(),
            "strict exclusive must not fall through to the shared sink"
        );
        match status.lock().expect("exclusive status mutex").clone() {
            ExclusiveStatus::Unavailable(reason) => assert_eq!(reason, "device in use"),
            other => panic!("strict should report Unavailable, got {other:?}"),
        }
    }

    /// Non-strict keeps playing but records WHY it isn't bit-perfect, so the UI
    /// can show the reason instead of a bare "shared".
    #[test]
    fn non_strict_exclusive_falls_back_and_keeps_the_reason() {
        let status = Arc::new(Mutex::new(ExclusiveStatus::Shared));
        let outcome = record_exclusive_failure("no exclusive format".into(), false, &status);
        assert!(
            outcome.is_ok(),
            "non-strict exclusive should continue to the shared sink"
        );
        match status.lock().expect("exclusive status mutex").clone() {
            ExclusiveStatus::FellBack(reason) => assert_eq!(reason, "no exclusive format"),
            other => panic!("non-strict should report FellBack, got {other:?}"),
        }
    }

    /// On a platform with NO exclusive backend wired, a request resolves to the
    /// platform reason, the string the Settings card surfaces. Pins the
    /// behaviour that such a build degrades honestly rather than reporting
    /// `Active` for what is really a shared stream.
    ///
    /// Windows (WASAPI), Linux (ALSA) and macOS (CoreAudio) all have one, so this
    /// is the everywhere-else case; `exclusive_is_advertised_exactly_where_a_backend_exists`
    /// is what keeps the two lists from drifting apart.
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    #[test]
    fn with_no_backend_the_exclusive_reason_names_the_platform() {
        let status = Arc::new(Mutex::new(ExclusiveStatus::Shared));
        let _ = record_exclusive_failure(NO_EXCLUSIVE_BACKEND.to_string(), false, &status);
        match status.lock().expect("exclusive status mutex").clone() {
            ExclusiveStatus::FellBack(reason) => assert!(
                reason.contains("not available on this platform"),
                "got {reason:?}"
            ),
            other => panic!("expected FellBack, got {other:?}"),
        }
    }

    /// REAL-DEVICE guard for the shared-sink live switch: the create-then-drop order
    /// [`crate`]'s `maybe_apply_output_change` uses for a SHARED sink must not glitch the
    /// device. We open a second shared device sink WHILE the first is still alive (shared
    /// mode allows multiple clients per device), then drop the first, the new sink must
    /// keep accepting audio. This is the exact sequence that failed with cpal
    /// "the requested device is no longer available" when the old release-first code
    /// dropped the shared stream before re-opening it on the same device. Writes SILENCE
    /// only (inaudible). Ignored: needs a real output device:
    ///   `cargo test -p soul-player-host shared_device -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a real output device; run with --ignored"]
    fn shared_device_create_then_drop_keeps_the_new_sink_working() {
        let status = Arc::new(Mutex::new(ExclusiveStatus::Shared));
        let mut old = match open_device_sink(None, false, false, &status) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("no usable output device ({e:#}), skipping");
                return;
            }
        };
        let si = old.stream_info();
        let silence = vec![0i32; si.channels as usize * 2048];
        old.write(&silence).expect("old shared sink plays silence");

        // Build the NEW sink FIRST, while the old is still alive (the fix's order).
        let mut new = open_device_sink(None, false, false, &status)
            .expect("open a second shared sink while the first is still alive");
        new.write(&silence)
            .expect("new sink plays while the old is alive");

        // Drop the old sink; the new one must keep working, no device-unavailable gap.
        drop(old);
        for i in 0..16 {
            new.write(&silence).unwrap_or_else(|e| {
                panic!("new sink failed after the old was dropped (block {i}): {e:?}")
            });
        }
        new.flush().expect("new sink flush");
        println!("OK: shared create-then-drop on the real device kept the new sink working");
    }
}
