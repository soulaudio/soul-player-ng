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

use std::sync::{Arc, Mutex};

use soul_audio_core::{Biquad, Equalizer, Sink, SinkError, StreamInfo};
// The parametric-EQ *design* (RBJ coefficients + magnitude response) and the band
// model live in the shared **no_std** `soul-audio-eq` crate, so the DAP firmware
// reuses the exact same pipeline. This host module is the std half: the live
// config, its shared handle, and the `Sink` decorator that applies it + feeds the
// spectrum analyzer.
pub use soul_audio_eq::{
    Band, BandKind, FREQ_MAX, FREQ_MIN, GAIN_LIMIT_DB, MAX_BANDS, Q_MAX, Q_MIN, band_magnitude_db,
    default_bands, design, eq_magnitude_db,
};

use crate::analyzer::{Analyzer, SpectrumShared};

/// Canonical sample bound: the deck normalises to signed 24-bit (±2²³).
const CANONICAL_MAX: f32 = 8_388_607.0;

/// The live EQ settings the UI writes and the [`EqSink`] reads: the parametric
/// bands, a preamp (headroom for boosts), an enable flag, and a `generation`
/// counter the sink watches to know when to redesign its filters.
#[derive(Debug, Clone, PartialEq)]
pub struct EqConfig {
    /// The parametric bands (0..=[`MAX_BANDS`]), applied as a series cascade.
    pub bands: Vec<Band>,
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
            bands: default_bands().to_vec(),
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

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A [`Sink`] decorator that applies the parametric EQ to each block before handing
/// it to the inner (device) sink. Runs at the device rate (fixed), so the band
/// coefficients are stable. Reads [`EqShared`] each block and redesigns when it
/// changes; a flat/off config is a bit-exact passthrough.
pub struct EqSink<K: Sink> {
    inner: K,
    eq: Equalizer<MAX_BANDS>,
    shared: EqShared,
    applied_gen: Option<u64>,
    preamp_lin: f32,
    rate: u32,
    channels: usize,
    buf_f32: Vec<f32>,
    buf_i32: Vec<i32>,
    analyzer: Analyzer,
}

impl<K: Sink> EqSink<K> {
    /// Wrap `inner`, reading settings from `shared` and publishing the analyzer
    /// spectrum to `spectrum`. The band filters and the analyzer are set up for
    /// `inner`'s sample rate.
    pub fn new(inner: K, shared: EqShared, spectrum: SpectrumShared) -> Self {
        let info = inner.stream_info();
        let rate = info.sample_rate;
        Self {
            inner,
            eq: Equalizer::new(),
            shared,
            applied_gen: None,
            preamp_lin: 1.0,
            rate,
            channels: usize::from(info.channels.max(1)),
            buf_f32: Vec::new(),
            buf_i32: Vec::new(),
            analyzer: Analyzer::new(rate as f32, spectrum),
        }
    }

    /// Redesign from the shared config if it changed since the last block; returns
    /// whether the EQ is currently active (so `write` can fast-path passthrough).
    fn sync(&mut self) -> bool {
        let cfg = self.shared.lock().expect("eq config mutex").clone();
        if self.applied_gen != Some(cfg.generation) {
            self.applied_gen = Some(cfg.generation);
            // Design each active band; bypass the remaining fixed slots.
            for i in 0..MAX_BANDS {
                let filter = match cfg.bands.get(i) {
                    Some(band) => design(band, self.rate as f32),
                    None => Biquad::bypass(),
                };
                self.eq.set_band(i, filter);
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
        // Feed the analyzer the pre-EQ signal (the track's own content) whether or
        // not the EQ is engaged — the UI overlays the EQ response to show the effect.
        self.analyzer.feed(frames, self.channels);
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
    use crate::analyzer::SpectrumFrame;
    use soul_audio_core::SampleFormat;
    use std::f32::consts::PI;

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

    /// Build an [`EqSink`] over `inner` with a throwaway analyzer buffer (the tests
    /// assert on the audio path, not the spectrum).
    fn eq_sink(inner: Capture, shared: EqShared) -> EqSink<Capture> {
        EqSink::new(
            inner,
            shared,
            Arc::new(Mutex::new(SpectrumFrame::default())),
        )
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

    // ---- EqSink behaviour -----------------------------------------------------

    #[test]
    fn disabled_or_flat_is_a_bit_exact_passthrough() {
        let input = sine(1000.0, FS, 4096, 3_000_000.0);
        // Disabled.
        let off = Arc::new(Mutex::new(EqConfig::default()));
        let mut sink = eq_sink(
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
        let mut sink = eq_sink(
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
        let mut sink = eq_sink(
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
        let mut sink = eq_sink(
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
        let mut sink = eq_sink(
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
        let mut sink = eq_sink(
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
