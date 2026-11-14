//! The desktop **graphic equalizer**: 10 ISO-octave peaking bands applied at the
//! output sink, after the deck's ReplayGain + resample (the researched chain
//! order: ReplayGain → preamp → EQ → output). The `no_std` core provides the
//! real-time biquad primitive ([`soul_audio_core::Equalizer`]); this host module
//! does the transcendental coefficient **design** (RBJ "Audio EQ Cookbook") and
//! wraps the device sink in an [`EqSink`] that filters each block.
//!
//! A flat or disabled EQ is a **bit-exact passthrough** — the frames go straight
//! to the device, so turning the EQ off restores an untouched signal path.
#![forbid(unsafe_code)]

use std::f32::consts::PI;
use std::sync::{Arc, Mutex};

use soul_audio_core::{Biquad, Equalizer, Sink, SinkError, StreamInfo};

/// Number of graphic-EQ bands.
pub const BANDS: usize = 10;

/// ISO-octave band center frequencies (Hz) — the standard 10-band graphic-EQ set.
pub const BAND_FREQS: [f32; BANDS] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Per-band peaking Q (Peace's peaking default; ≈√2 gives sensible octave
/// overlap).
const BAND_Q: f32 = 1.41;

/// Gain limit (± dB) for each band and the preamp — the UI clamps to this.
pub const GAIN_LIMIT_DB: f32 = 12.0;

/// Canonical sample bound: the deck normalises to signed 24-bit (±2²³).
const CANONICAL_MAX: f32 = 8_388_607.0;

/// The live EQ settings the UI writes and the [`EqSink`] reads: per-band gains, a
/// preamp (headroom for boosts), an enable flag, and a `generation` counter the
/// sink watches to know when to redesign its filters.
#[derive(Debug, Clone, PartialEq)]
pub struct EqConfig {
    /// Per-band gain in dB (index ↔ [`BAND_FREQS`]).
    pub gains_db: [f32; BANDS],
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
            gains_db: [0.0; BANDS],
            preamp_db: 0.0,
            enabled: false,
            generation: 0,
        }
    }
}

impl EqConfig {
    /// Whether the settings would actually change the signal (enabled, with a
    /// non-trivial preamp or some non-zero band). When `false` the sink passes the
    /// frames through untouched.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.enabled
            && (self.preamp_db.abs() > 1e-3 || self.gains_db.iter().any(|g| g.abs() > 1e-3))
    }
}

/// A shared handle to the live [`EqConfig`]: the `PlayerHandle` writes it, the
/// [`EqSink`] (on the pump thread) reads it and redesigns when `generation` moves.
pub type EqShared = Arc<Mutex<EqConfig>>;

/// RBJ "Audio EQ Cookbook" peaking coefficients (already `a0`-normalised) as
/// `[b0, b1, b2, a1, a2]`, or `None` (a flat passthrough) at zero gain, at/above
/// Nyquist, or a non-positive Q. Shared by [`design_peaking`] (which applies them)
/// and [`eq_magnitude_db`] (which evaluates the response).
fn peaking_coeffs(f0: f32, q: f32, gain_db: f32, fs: f32) -> Option<[f32; 5]> {
    if gain_db.abs() < 1e-3 || f0 <= 0.0 || f0 >= fs / 2.0 || q <= 0.0 {
        return None;
    }
    let a = 10f32.powf(gain_db / 40.0);
    let w0 = 2.0 * PI * f0 / fs;
    let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
    let alpha = sin_w0 / (2.0 * q);
    let a0 = 1.0 + alpha / a;
    Some([
        (1.0 + alpha * a) / a0,
        (-2.0 * cos_w0) / a0,
        (1.0 - alpha * a) / a0,
        (-2.0 * cos_w0) / a0,
        (1.0 - alpha / a) / a0,
    ])
}

/// The RBJ peaking biquad for a band — the transcendental design the `no_std`
/// core delegates to the std host. Flat ([`Biquad::bypass`]) at zero gain / above
/// Nyquist.
#[must_use]
pub fn design_peaking(f0: f32, q: f32, gain_db: f32, fs: f32) -> Biquad {
    match peaking_coeffs(f0, q, gain_db, fs) {
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

/// The **combined graphic-EQ magnitude response** (dB) at `freq` Hz for a device
/// rate `fs`: the preamp plus each band's peaking response (cascaded filters
/// multiply in magnitude = add in dB). This is the curve the UI draws.
#[must_use]
pub fn eq_magnitude_db(gains_db: &[f32; BANDS], preamp_db: f32, freq: f32, fs: f32) -> f32 {
    let w = 2.0 * PI * freq / fs;
    let mut db = preamp_db;
    for (i, &g) in gains_db.iter().enumerate() {
        if let Some(c) = peaking_coeffs(BAND_FREQS[i], BAND_Q, g, fs) {
            db += biquad_mag_db(c, w);
        }
    }
    db
}

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A [`Sink`] decorator that applies the graphic EQ to each block before handing
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
            for (i, &g) in cfg.gains_db.iter().enumerate() {
                self.eq.set_band(
                    i,
                    design_peaking(BAND_FREQS[i], BAND_Q, g, self.rate as f32),
                );
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

    /// Peak magnitude over the steady-state tail (second half, past the filter
    /// transient).
    fn tail_peak(samples: &[i32]) -> f32 {
        samples[samples.len() / 2..]
            .iter()
            .map(|&x| x.unsigned_abs())
            .max()
            .unwrap_or(0) as f32
    }

    fn shared_with(gains: impl FnOnce(&mut EqConfig)) -> EqShared {
        let mut cfg = EqConfig {
            enabled: true,
            generation: 1,
            ..EqConfig::default()
        };
        gains(&mut cfg);
        Arc::new(Mutex::new(cfg))
    }

    #[test]
    fn design_zero_gain_or_above_nyquist_is_bypass() {
        assert_eq!(
            design_peaking(1000.0, 1.41, 0.0, 48_000.0),
            Biquad::bypass()
        );
        assert_eq!(
            design_peaking(16_000.0, 1.41, 6.0, 30_000.0),
            Biquad::bypass(),
            "a band at/above Nyquist can't be realised — bypass it"
        );
        assert_ne!(
            design_peaking(1000.0, 1.41, 6.0, 48_000.0),
            Biquad::bypass(),
            "a real boost is not a bypass"
        );
    }

    #[test]
    fn magnitude_response_tracks_the_bands_and_preamp() {
        let fs = 48_000.0;
        let mut gains = [0.0f32; BANDS];
        // Flat + no preamp → 0 dB everywhere.
        assert!(eq_magnitude_db(&gains, 0.0, 1000.0, fs).abs() < 0.01);
        // A pure preamp is a flat offset at every frequency.
        assert!((eq_magnitude_db(&gains, -3.0, 500.0, fs) + 3.0).abs() < 0.01);
        // +6 dB at the 1 kHz band (index 5): ~+6 dB there, ~flat an octave away.
        gains[5] = 6.0;
        assert!((eq_magnitude_db(&gains, 0.0, 1000.0, fs) - 6.0).abs() < 0.3);
        assert!(eq_magnitude_db(&gains, 0.0, 125.0, fs).abs() < 1.0);
        assert!(eq_magnitude_db(&gains, 0.0, 8000.0, fs).abs() < 1.0);
        // A preamp adds on top of the band boost.
        assert!((eq_magnitude_db(&gains, 2.0, 1000.0, fs) - 8.0).abs() < 0.3);
    }

    #[test]
    fn disabled_or_flat_is_a_bit_exact_passthrough() {
        let fs = 48_000.0;
        let input = sine(1000.0, fs, 4096, 3_000_000.0);
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
        // Enabled but all-flat (0 dB everywhere).
        let flat = shared_with(|_| {});
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            flat,
        );
        sink.write(&input).unwrap();
        assert_eq!(sink.inner.out, input, "an all-0 dB EQ is bit-exact");
    }

    #[test]
    fn boosting_a_band_raises_its_center_frequency_by_that_gain() {
        let fs = 48_000.0;
        let n = 24_000; // 0.5 s
        let amp = 3_000_000.0; // ~0.36 FS — headroom for a +6 dB boost
        // +6 dB at the 1 kHz band (index 5).
        let shared = shared_with(|c| c.gains_db[5] = 6.0);
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );
        let input = sine(1000.0, fs, n, amp);
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
    fn a_band_leaves_distant_frequencies_alone() {
        let fs = 48_000.0;
        let n = 24_000;
        let amp = 3_000_000.0;
        // +10 dB at 1 kHz, but drive a 100 Hz tone — well outside that band.
        let shared = shared_with(|c| c.gains_db[5] = 10.0);
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );
        let input = sine(100.0, fs, n, amp);
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
        // Left = 1 kHz tone, right = silence, +6 dB @ 1 kHz. The right channel must
        // stay silent (independent filter state), the left must be boosted.
        let fs = 48_000.0;
        let n = 12_000;
        let amp = 3_000_000.0;
        let left = sine(1000.0, fs, n, amp);
        let mut interleaved = Vec::with_capacity(n * 2);
        for &l in &left {
            interleaved.push(l);
            interleaved.push(0); // right silent
        }
        let shared = shared_with(|c| c.gains_db[5] = 6.0);
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
        let fs = 48_000.0;
        let input = sine(1000.0, fs, 24_000, 3_000_000.0);
        let shared = Arc::new(Mutex::new(EqConfig::default())); // starts off
        let mut sink = EqSink::new(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared.clone(),
        );
        sink.write(&input).unwrap();
        assert_eq!(sink.inner.out.len(), input.len());
        let flat_peak = tail_peak(&sink.inner.out);
        // Flip on a +6 dB boost without rebuilding the sink.
        {
            let mut c = shared.lock().unwrap();
            c.enabled = true;
            c.gains_db[5] = 6.0;
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
