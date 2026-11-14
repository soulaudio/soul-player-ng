//! The desktop **parametric equalizer**: a cascade of fully parametric bands
//! (bell / shelf / cut / notch, each with its own frequency, gain and Q) applied
//! at the output sink, after the deck's ReplayGain + resample (the researched
//! chain order: ReplayGain → preamp → EQ → output). The `no_std` core provides
//! the real-time biquad primitive ([`soul_audio_core::Equalizer`]); this host
//! module does the transcendental coefficient **design** (RBJ "Audio EQ
//! Cookbook") and wraps the device sink in an [`EqSink`] that filters each block.
//!
//! A flat or disabled EQ is a **bit-exact passthrough** — the frames go straight
//! to the device, so turning the EQ off restores an untouched signal path.
#![forbid(unsafe_code)]

use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

use soul_audio_core::{Biquad, Equalizer, Sink, SinkError, StreamInfo};

/// Number of parametric EQ bands.
pub const BANDS: usize = 8;

/// Gain limit (± dB) for each band and the preamp — the UI clamps to this.
pub const GAIN_LIMIT_DB: f32 = 12.0;

/// Q (resonance / bandwidth) limits the UI clamps to. The low end is a very wide
/// band; the high end is surgical.
pub const Q_MIN: f32 = 0.3;
pub const Q_MAX: f32 = 12.0;

/// Displayed/draggable frequency range (Hz).
pub const FREQ_MIN: f32 = 20.0;
pub const FREQ_MAX: f32 = 20_000.0;

/// Canonical sample bound: the deck normalises to signed 24-bit (±2²³).
const CANONICAL_MAX: f32 = 8_388_607.0;

/// A band's filter shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
    /// Boost/cut everything below the corner (gain applies).
    LowShelf,
    /// Boost/cut a bell around the frequency (gain applies).
    Peaking,
    /// Boost/cut everything above the corner (gain applies).
    HighShelf,
    /// High-pass: attenuate below the corner (gain ignored).
    LowCut,
    /// Low-pass: attenuate above the corner (gain ignored).
    HighCut,
    /// Reject a narrow band around the frequency (gain ignored).
    Notch,
}

impl BandKind {
    /// Whether this shape uses the `gain_db` field. Cuts and notches don't.
    #[must_use]
    pub fn uses_gain(self) -> bool {
        matches!(self, Self::LowShelf | Self::Peaking | Self::HighShelf)
    }
}

/// One parametric band: its shape, corner/centre frequency (Hz), gain (dB, for the
/// gain shapes), Q, and whether it is engaged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub kind: BandKind,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
    pub enabled: bool,
}

impl Band {
    /// Whether this band, as configured, actually changes the signal: an engaged
    /// gain shape with a non-trivial gain, or any engaged cut/notch.
    #[must_use]
    pub fn affects(&self) -> bool {
        self.enabled
            && self.freq > 0.0
            && (if self.kind.uses_gain() {
                self.gain_db.abs() > 1e-3
            } else {
                true
            })
    }
}

/// The default 8-band layout: a low shelf, six bells spread over the spectrum, and
/// a high shelf — all flat (0 dB), so the default EQ is a passthrough.
#[must_use]
pub fn default_bands() -> [Band; BANDS] {
    const LAYOUT: [(BandKind, f32, f32); BANDS] = [
        (BandKind::LowShelf, 80.0, 0.7),
        (BandKind::Peaking, 150.0, 1.0),
        (BandKind::Peaking, 350.0, 1.0),
        (BandKind::Peaking, 800.0, 1.0),
        (BandKind::Peaking, 2000.0, 1.0),
        (BandKind::Peaking, 4500.0, 1.0),
        (BandKind::Peaking, 9000.0, 1.0),
        (BandKind::HighShelf, 15_000.0, 0.7),
    ];
    LAYOUT.map(|(kind, freq, q)| Band {
        kind,
        freq,
        gain_db: 0.0,
        q,
        enabled: true,
    })
}

/// The live EQ settings the UI writes and the [`EqSink`] reads: the parametric
/// bands, a preamp (headroom for boosts), an enable flag, and a `generation`
/// counter the sink watches to know when to redesign its filters.
#[derive(Debug, Clone, PartialEq)]
pub struct EqConfig {
    /// The parametric bands, applied as a series cascade.
    pub bands: [Band; BANDS],
    /// A global pre-gain in dB applied before the bands (negative to leave
    /// headroom so boosts don't clip).
    pub preamp_db: f32,
    /// Master enable. Disabled is a bit-exact passthrough.
    pub enabled: bool,
    /// Bumped on every change so the sink knows to redesign.
    pub generation: u64,
}

impl Default for EqConfig {
    fn default() -> Self {
        Self {
            bands: default_bands(),
            preamp_db: 0.0,
            enabled: false,
            generation: 0,
        }
    }
}

impl EqConfig {
    /// Whether the settings would actually change the signal (enabled, with a
    /// non-trivial preamp or some engaged band). When `false` the sink passes the
    /// frames through untouched.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.enabled && (self.preamp_db.abs() > 1e-3 || self.bands.iter().any(Band::affects))
    }
}

/// A shared handle to the live [`EqConfig`]: the `PlayerHandle` writes it, the
/// [`EqSink`] (on the pump thread) reads it and redesigns when `generation` moves.
pub type EqShared = Arc<Mutex<EqConfig>>;

/// RBJ "Audio EQ Cookbook" biquad coefficients, already `a0`-normalised, as
/// `[b0, b1, b2, a1, a2]` — or `None` (a flat passthrough) for a band that cannot
/// or should not filter: out of range, non-positive Q, or a gain shape at ~0 dB.
///
/// Shelves are parameterised by Q (`alpha = sin(w0)/(2Q)`) rather than the
/// cookbook's shelf-slope S, which is the modern parametric-EQ convention (at
/// `Q = 1/√2` it matches the classic `S = 1` shelf). Shared by [`design`] (which
/// applies the coefficients) and [`band_magnitude_db`] (which evaluates them).
fn coeffs(kind: BandKind, f0: f32, gain_db: f32, q: f32, fs: f32) -> Option<[f32; 5]> {
    if f0 <= 0.0 || f0 >= fs / 2.0 || q <= 0.0 {
        return None;
    }
    if kind.uses_gain() && gain_db.abs() < 1e-3 {
        return None; // a flat bell/shelf is a passthrough
    }
    let w0 = 2.0 * PI * f0 / fs;
    let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
    let alpha = sin_w0 / (2.0 * q);
    // `A` is the *amplitude* √(linear gain): exponent /40, not /20 (the classic
    // RBJ transcription gotcha).
    let a = 10f32.powf(gain_db / 40.0);

    let (b0, b1, b2, a0, a1, a2) = match kind {
        BandKind::Peaking => (
            1.0 + alpha * a,
            -2.0 * cos_w0,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w0,
            1.0 - alpha / a,
        ),
        BandKind::LowShelf => {
            let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
                a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
                (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
            )
        }
        BandKind::HighShelf => {
            let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
            (
                a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
                a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
                (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
            )
        }
        BandKind::LowCut => (
            (1.0 + cos_w0) / 2.0,
            -(1.0 + cos_w0),
            (1.0 + cos_w0) / 2.0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        ),
        BandKind::HighCut => (
            (1.0 - cos_w0) / 2.0,
            1.0 - cos_w0,
            (1.0 - cos_w0) / 2.0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        ),
        BandKind::Notch => (
            1.0,
            -2.0 * cos_w0,
            1.0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        ),
    };
    Some([b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0])
}

/// The RBJ biquad for a band — the transcendental design the `no_std` core
/// delegates to the std host. Flat ([`Biquad::bypass`]) for a disabled/flat band
/// or one out of range.
#[must_use]
pub fn design(band: &Band, fs: f32) -> Biquad {
    if !band.enabled {
        return Biquad::bypass();
    }
    match coeffs(band.kind, band.freq, band.gain_db, band.q, fs) {
        Some([b0, b1, b2, a1, a2]) => Biquad::new(b0, b1, b2, a1, a2),
        None => Biquad::bypass(),
    }
}

/// Magnitude response (dB) of a biquad `[b0, b1, b2, a1, a2]` at normalised
/// angular frequency `w` (0..π): `20·log₁₀(|H(e^jw)|)`.
fn biquad_mag_db(c: [f32; 5], w: f32) -> f32 {
    let (cw, sw) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
    let num_re = c[0] + c[1] * cw + c[2] * c2;
    let num_im = -(c[1] * sw + c[2] * s2);
    let den_re = 1.0 + c[3] * cw + c[4] * c2;
    let den_im = -(c[3] * sw + c[4] * s2);
    let num = (num_re * num_re + num_im * num_im).sqrt();
    let den = (den_re * den_re + den_im * den_im).sqrt();
    if den <= 0.0 {
        0.0
    } else {
        20.0 * (num / den).max(1e-9).log10()
    }
}

/// A single band's magnitude response (dB) at `freq` Hz for device rate `fs` — the
/// thin per-band curve the UI draws. `0.0` (flat) for a band that doesn't filter.
#[must_use]
pub fn band_magnitude_db(band: &Band, freq: f32, fs: f32) -> f32 {
    if !band.enabled {
        return 0.0;
    }
    match coeffs(band.kind, band.freq, band.gain_db, band.q, fs) {
        Some(c) => biquad_mag_db(c, 2.0 * PI * freq / fs),
        None => 0.0,
    }
}

/// The **combined parametric-EQ magnitude response** (dB) at `freq` Hz for a device
/// rate `fs`: the preamp plus each band's response (cascaded filters multiply in
/// magnitude = add in dB). This is the thick overall curve the UI draws.
#[must_use]
pub fn eq_magnitude_db(bands: &[Band; BANDS], preamp_db: f32, freq: f32, fs: f32) -> f32 {
    let mut db = preamp_db;
    for band in bands {
        db += band_magnitude_db(band, freq, fs);
    }
    db
}

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A [`Sink`] decorator that applies the parametric EQ to each block before handing
/// it to the inner (device) sink. Runs at the device rate (fixed), so the band
/// coefficients are stable. Reads [`EqShared`] each block and redesigns when it
/// changes; a flat/off config is a bit-exact passthrough.
pub struct EqSink<K: Sink> {
    inner: K,
    eq: Equalizer<BANDS>,
    shared: EqShared,
    applied_gen: Option<u64>,
    preamp_lin: f32,
    rate: u32,
    channels: usize,
    buf_f32: Vec<f32>,
    buf_i32: Vec<i32>,
}

impl<K: Sink> EqSink<K> {
    /// Wrap `inner`, reading settings from `shared`. The band filters are designed
    /// for `inner`'s sample rate.
    pub fn new(inner: K, shared: EqShared) -> Self {
        let info = inner.stream_info();
        Self {
            inner,
            eq: Equalizer::new(),
            shared,
            applied_gen: None,
            preamp_lin: 1.0,
            rate: info.sample_rate,
            channels: usize::from(info.channels.max(1)),
            buf_f32: Vec::new(),
            buf_i32: Vec::new(),
        }
    }

    /// Redesign from the shared config if it changed since the last block; returns
    /// whether the EQ is currently active (so `write` can fast-path passthrough).
    fn sync(&mut self) -> bool {
        let cfg = self.shared.lock().expect("eq config mutex").clone();
        if self.applied_gen != Some(cfg.generation) {
            self.applied_gen = Some(cfg.generation);
            for (i, band) in cfg.bands.iter().enumerate() {
                self.eq.set_band(i, design(band, self.rate as f32));
            }
            self.preamp_lin = db_to_lin(cfg.preamp_db);
            self.eq.set_enabled(cfg.enabled);
        }
        cfg.is_active()
    }
}

impl<K: Sink> Sink for EqSink<K> {
    fn stream_info(&self) -> StreamInfo {
        self.inner.stream_info()
    }

    fn write(&mut self, frames: &[i32]) -> Result<(), SinkError> {
        if !self.sync() {
            // Flat/off → bit-exact passthrough (no i32↔f32 round-trip).
            return self.inner.write(frames);
        }
        self.buf_f32.clear();
        self.buf_f32
            .extend(frames.iter().map(|&x| x as f32 * self.preamp_lin));
        self.eq.process(&mut self.buf_f32, self.channels);
        self.buf_i32.clear();
        self.buf_i32.extend(
            self.buf_f32
                .iter()
                .map(|&y| y.round().clamp(-CANONICAL_MAX - 1.0, CANONICAL_MAX) as i32),
        );
        self.inner.write(&self.buf_i32)
    }

    fn flush(&mut self) -> Result<(), SinkError> {
        self.inner.flush()
    }

    fn discard(&mut self) -> Result<(), SinkError> {
        // A seek is a discontinuity — drop the filter tail so it can't ring into
        // the new position.
        self.eq.reset();
        self.inner.discard()
    }

    fn set_volume(&mut self, volume: f32) {
        self.inner.set_volume(volume);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soul_audio_core::SampleFormat;

    const FS: f32 = 48_000.0;

    fn bell(freq: f32, gain_db: f32, q: f32) -> Band {
        Band {
            kind: BandKind::Peaking,
            freq,
            gain_db,
            q,
            enabled: true,
        }
    }

    /// A sink that captures everything written to it.
    struct Capture {
        info: StreamInfo,
        out: Vec<i32>,
    }
    impl Sink for Capture {
        fn stream_info(&self) -> StreamInfo {
            self.info
        }
        fn write(&mut self, frames: &[i32]) -> Result<(), SinkError> {
            self.out.extend_from_slice(frames);
            Ok(())
        }
        fn flush(&mut self) -> Result<(), SinkError> {
            Ok(())
        }
    }

    fn info(rate: u32, channels: u16) -> StreamInfo {
        StreamInfo {
            sample_rate: rate,
            channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }

    /// A mono `freq`-Hz sine of `n` samples at amplitude `amp` (24-bit scale).
    fn sine(freq: f32, fs: f32, n: usize, amp: f32) -> Vec<i32> {
        (0..n)
            .map(|i| (amp * (2.0 * PI * freq * i as f32 / fs).sin()).round() as i32)
            .collect()
    }

    /// Peak magnitude over the steady-state tail (second half, past the transient).
    fn tail_peak(samples: &[i32]) -> f32 {
        samples[samples.len() / 2..]
            .iter()
            .map(|&x| x.unsigned_abs())
            .max()
            .unwrap_or(0) as f32
    }

    fn shared_with(edit: impl FnOnce(&mut EqConfig)) -> EqShared {
        let mut cfg = EqConfig {
            enabled: true,
            generation: 1,
            ..EqConfig::default()
        };
        edit(&mut cfg);
        Arc::new(Mutex::new(cfg))
    }

    // ---- coefficient / magnitude design (RBJ) --------------------------------

    #[test]
    fn a_flat_or_out_of_range_band_is_bypass() {
        assert_eq!(design(&bell(1000.0, 0.0, 1.0), FS), Biquad::bypass());
        assert_eq!(
            design(&bell(16_000.0, 6.0, 1.0), 30_000.0),
            Biquad::bypass(),
            "a band at/above Nyquist can't be realised"
        );
        let mut disabled = bell(1000.0, 6.0, 1.0);
        disabled.enabled = false;
        assert_eq!(design(&disabled, FS), Biquad::bypass());
        assert_ne!(design(&bell(1000.0, 6.0, 1.0), FS), Biquad::bypass());
    }

    #[test]
    fn peaking_hits_its_gain_at_centre_and_is_flat_far_away() {
        let b = bell(1000.0, 6.0, 1.0);
        assert!((band_magnitude_db(&b, 1000.0, FS) - 6.0).abs() < 0.2);
        assert!(band_magnitude_db(&b, 125.0, FS).abs() < 1.0);
        assert!(band_magnitude_db(&b, 8000.0, FS).abs() < 1.0);
        // A cut is symmetric.
        let c = bell(1000.0, -6.0, 1.0);
        assert!((band_magnitude_db(&c, 1000.0, FS) + 6.0).abs() < 0.2);
    }

    #[test]
    fn q_controls_peaking_bandwidth() {
        // Half an octave out (≈1.4 kHz) a narrow band has fallen off far more than
        // a wide one.
        let wide = band_magnitude_db(&bell(1000.0, 10.0, 0.5), 1400.0, FS);
        let narrow = band_magnitude_db(&bell(1000.0, 10.0, 6.0), 1400.0, FS);
        assert!(
            wide > narrow + 3.0,
            "a wider band (low Q) should still boost 1.4 kHz far more than a narrow one: wide {wide:.2} narrow {narrow:.2}"
        );
    }

    #[test]
    fn shelves_lift_one_side_and_leave_the_other() {
        let fs = FS;
        let low = Band {
            kind: BandKind::LowShelf,
            freq: 120.0,
            gain_db: 6.0,
            q: 0.7,
            enabled: true,
        };
        assert!(
            (band_magnitude_db(&low, 30.0, fs) - 6.0).abs() < 0.6,
            "low shelf reaches +6 dB in the bass"
        );
        assert!(
            band_magnitude_db(&low, 10_000.0, fs).abs() < 0.5,
            "low shelf leaves the treble alone"
        );
        let high = Band {
            kind: BandKind::HighShelf,
            freq: 8000.0,
            gain_db: -6.0,
            q: 0.7,
            enabled: true,
        };
        assert!(
            (band_magnitude_db(&high, 18_000.0, fs) + 6.0).abs() < 0.8,
            "high shelf reaches −6 dB on top"
        );
        assert!(
            band_magnitude_db(&high, 200.0, fs).abs() < 0.5,
            "high shelf leaves the bass alone"
        );
    }

    #[test]
    fn cuts_attenuate_the_stopband_and_pass_the_passband() {
        let fs = FS;
        let hp = Band {
            kind: BandKind::LowCut,
            freq: 500.0,
            gain_db: 0.0,
            q: 0.707,
            enabled: true,
        };
        assert!(
            band_magnitude_db(&hp, 60.0, fs) < -15.0,
            "low-cut kills the deep bass"
        );
        assert!(
            (band_magnitude_db(&hp, 500.0, fs) + 3.0).abs() < 1.0,
            "≈−3 dB at the corner"
        );
        assert!(
            band_magnitude_db(&hp, 8000.0, fs).abs() < 0.3,
            "passes the highs"
        );
        let lp = Band {
            kind: BandKind::HighCut,
            freq: 2000.0,
            gain_db: 0.0,
            q: 0.707,
            enabled: true,
        };
        assert!(
            band_magnitude_db(&lp, 16_000.0, fs) < -15.0,
            "high-cut kills the top"
        );
        assert!(
            band_magnitude_db(&lp, 200.0, fs).abs() < 0.3,
            "passes the lows"
        );
    }

    #[test]
    fn notch_rejects_its_centre_and_passes_around_it() {
        let n = Band {
            kind: BandKind::Notch,
            freq: 1000.0,
            gain_db: 0.0,
            q: 4.0,
            enabled: true,
        };
        assert!(
            band_magnitude_db(&n, 1000.0, FS) < -30.0,
            "a notch kills its centre"
        );
        assert!(
            band_magnitude_db(&n, 250.0, FS).abs() < 0.5,
            "and passes well below"
        );
        assert!(
            band_magnitude_db(&n, 4000.0, FS).abs() < 0.5,
            "and well above"
        );
    }

    #[test]
    fn combined_response_sums_bands_and_preamp() {
        let mut bands = default_bands();
        // Bell index 4 is 2 kHz; set it to +6, and the low shelf (0) to +3.
        bands[4].gain_db = 6.0;
        bands[0].gain_db = 3.0;
        // Around 2 kHz ≈ the bell boost; deep bass ≈ the shelf; plus a −2 preamp.
        assert!((eq_magnitude_db(&bands, -2.0, 2000.0, FS) - 4.0).abs() < 0.6);
        assert!((eq_magnitude_db(&bands, -2.0, 30.0, FS) - 1.0).abs() < 0.8);
    }

    // ---- EqSink behaviour -----------------------------------------------------

    #[test]
    fn disabled_or_flat_is_a_bit_exact_passthrough() {
        let input = sine(1000.0, FS, 4096, 3_000_000.0);
        // Disabled.
        let off = Arc::new(Mutex::new(EqConfig::default()));
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            off,
        );
        sink.write(&input).unwrap();
        assert_eq!(sink.inner.out, input, "a disabled EQ is bit-exact");
        // Enabled but all-flat (default bands, 0 dB everywhere).
        let flat = shared_with(|_| {});
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            flat,
        );
        sink.write(&input).unwrap();
        assert_eq!(sink.inner.out, input, "an all-flat EQ is bit-exact");
    }

    #[test]
    fn boosting_a_band_raises_its_center_frequency_by_that_gain() {
        let n = 24_000; // 0.5 s
        let amp = 3_000_000.0; // ~0.36 FS — headroom for a +6 dB boost
        // +6 dB bell at 1 kHz on band index 3 (retune it to 1 kHz).
        let shared = shared_with(|c| {
            c.bands[3] = bell(1000.0, 6.0, 1.0);
        });
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );
        let input = sine(1000.0, FS, n, amp);
        let in_peak = tail_peak(&input);
        sink.write(&input).unwrap();
        let out_peak = tail_peak(&sink.inner.out);
        let gain = out_peak / in_peak;
        let want = db_to_lin(6.0); // ≈1.995×
        assert!(
            (gain / want - 1.0).abs() < 0.08,
            "a +6 dB band should ~double the 1 kHz amplitude: gain {gain:.3}, want {want:.3}"
        );
    }

    #[test]
    fn a_distant_frequency_is_left_alone() {
        let n = 24_000;
        let amp = 3_000_000.0;
        // +10 dB narrow bell at 1 kHz, but drive a 100 Hz tone.
        let shared = shared_with(|c| c.bands[3] = bell(1000.0, 10.0, 2.0));
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );
        let input = sine(100.0, FS, n, amp);
        let in_peak = tail_peak(&input);
        sink.write(&input).unwrap();
        let out_peak = tail_peak(&sink.inner.out);
        let gain = out_peak / in_peak;
        assert!(
            (gain - 1.0).abs() < 0.06,
            "a 1 kHz band should barely touch 100 Hz: gain {gain:.3}"
        );
    }

    #[test]
    fn stereo_channels_are_filtered_independently() {
        let n = 12_000;
        let amp = 3_000_000.0;
        let left = sine(1000.0, FS, n, amp);
        let mut interleaved = Vec::with_capacity(n * 2);
        for &l in &left {
            interleaved.push(l);
            interleaved.push(0); // right silent
        }
        let shared = shared_with(|c| c.bands[3] = bell(1000.0, 6.0, 1.0));
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 2),
                out: Vec::new(),
            },
            shared,
        );
        sink.write(&interleaved).unwrap();
        let out = &sink.inner.out;
        assert!(
            out.iter().skip(1).step_by(2).all(|&r| r == 0),
            "the silent right channel must stay silent (no inter-channel bleed)"
        );
        let out_left: Vec<i32> = out.iter().step_by(2).copied().collect();
        assert!(
            tail_peak(&out_left) > amp * 1.8,
            "the left channel's 1 kHz tone should be boosted ~+6 dB"
        );
    }

    #[test]
    fn a_generation_bump_is_picked_up_live() {
        let input = sine(1000.0, FS, 24_000, 3_000_000.0);
        let shared = Arc::new(Mutex::new(EqConfig::default())); // starts off
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared.clone(),
        );
        sink.write(&input).unwrap();
        let flat_peak = tail_peak(&sink.inner.out);
        // Flip on a +6 dB boost at 1 kHz without rebuilding the sink.
        {
            let mut c = shared.lock().unwrap();
            c.enabled = true;
            c.bands[3] = bell(1000.0, 6.0, 1.0);
            c.generation += 1;
        }
        sink.inner.out.clear();
        sink.write(&input).unwrap();
        assert!(
            tail_peak(&sink.inner.out) > flat_peak * 1.5,
            "a live generation bump must take effect on the next block"
        );
    }
}
