//! The desktop **DSP chain**: an ordered list of parametric-EQ units (each a
//! cascade of bell / shelf / cut / notch bands), applied at the output sink after
//! the deck's ReplayGain + resample. The `no_std` core provides the real-time
//! biquad primitive ([`soul_audio_core::Equalizer`]); the shared `soul-audio-eq`
//! crate designs the coefficients; this host module wraps the device sink in a
//! [`DspSink`] that runs the units in order and feeds the spectrum analyzer.
//!
//! An empty / flat / off chain is a **bit-exact passthrough** — the frames go
//! straight to the device, so a bypassed chain restores an untouched signal path.
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use soul_audio_core::{Biquad, Equalizer, Sink, SinkError, StreamInfo};
// The DSP-chain + audio-profile model, the band model and the parametric-EQ *design*
// all live in shared **no_std** crates (`soul-audio-dsp` over `soul-audio-eq`), so the
// DAP firmware reuses the exact same pipeline. This host module is only the std half:
// the shared live handle, and the `Sink` decorator that applies the chain + feeds the
// spectrum analyzer. Re-exported here so callers keep reaching them through the host.
pub use soul_audio_dsp::{
    Band, BandKind, DspChain, DspKind, DspUnit, EqUnit, FREQ_MAX, FREQ_MIN, GAIN_LIMIT_DB,
    MAX_BANDS, Profile, ProfileSet, Q_MAX, Q_MIN, band_magnitude_db, default_bands, design,
    eq_magnitude_db, format_autoeq, parse_autoeq,
};

use crate::analyzer::{Analyzer, SpectrumShared};

/// Canonical sample bound: the deck normalises to signed 24-bit (±2²³).
const CANONICAL_MAX: f32 = 8_388_607.0;

/// A shared handle to the live [`DspChain`]: the `PlayerHandle` writes it, the
/// [`DspSink`] (on the pump thread) reads it and redesigns when `generation` moves.
pub type DspShared = Arc<Mutex<DspChain>>;

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// A [`Sink`] decorator that applies the DSP **chain** (a series of EQ units) to
/// each block before handing it to the inner (device) sink. Runs at the device
/// rate (fixed), so the band coefficients are stable. Reads [`DspShared`] each
/// block and redesigns when it changes; an empty/flat/off chain is a bit-exact
/// passthrough. The analyzer always taps the pre-chain (track) signal.
pub struct DspSink<K: Sink> {
    inner: K,
    /// One equalizer per chain unit, in order.
    units: Vec<Equalizer<MAX_BANDS>>,
    /// Each unit's linear preamp (1.0 for a disabled unit, so it's a passthrough).
    preamps: Vec<f32>,
    shared: DspShared,
    applied_gen: Option<u64>,
    rate: u32,
    channels: usize,
    buf_f32: Vec<f32>,
    buf_i32: Vec<i32>,
    analyzer: Analyzer,
    /// Kept so [`reopen`](Sink::reopen) can rebuild the analyzer for a new device rate
    /// (its bin→frequency map is rate-dependent).
    spectrum: SpectrumShared,
}

impl<K: Sink> DspSink<K> {
    /// Wrap `inner`, reading the chain from `shared` and publishing the analyzer
    /// spectrum to `spectrum`. The filters and the analyzer are set up for `inner`'s
    /// sample rate.
    pub fn new(inner: K, shared: DspShared, spectrum: SpectrumShared) -> Self {
        let info = inner.stream_info();
        let rate = info.sample_rate;
        Self {
            inner,
            units: Vec::new(),
            preamps: Vec::new(),
            shared,
            applied_gen: None,
            rate,
            channels: usize::from(info.channels.max(1)),
            buf_f32: Vec::new(),
            buf_i32: Vec::new(),
            analyzer: Analyzer::new(rate as f32, spectrum.clone()),
            spectrum,
        }
    }

    /// Redesign from the shared chain if it changed since the last block; returns
    /// whether the chain is currently active (so `write` can fast-path passthrough).
    fn sync(&mut self) -> bool {
        let chain = self.shared.lock().expect("dsp chain mutex").clone();
        if self.applied_gen != Some(chain.generation) {
            self.applied_gen = Some(chain.generation);
            // Grow/shrink the equalizer bank to match the chain (keeping existing
            // units' filter state so an edit doesn't click).
            self.units.resize_with(chain.units.len(), Equalizer::new);
            self.preamps.resize(chain.units.len(), 1.0);
            for (i, unit) in chain.units.iter().enumerate() {
                match unit {
                    DspUnit::Eq(eq) => {
                        for b in 0..MAX_BANDS {
                            let filter = match eq.bands.get(b) {
                                Some(band) => design(band, self.rate as f32),
                                None => Biquad::bypass(),
                            };
                            self.units[i].set_band(b, filter);
                        }
                        self.units[i].set_enabled(eq.enabled);
                        // A disabled unit is a full passthrough — no preamp either.
                        self.preamps[i] = if eq.enabled {
                            db_to_lin(eq.preamp_db)
                        } else {
                            1.0
                        };
                    }
                }
            }
        }
        chain.is_active()
    }
}

impl<K: Sink> Sink for DspSink<K> {
    fn stream_info(&self) -> StreamInfo {
        self.inner.stream_info()
    }

    /// Forward the wrapped device's exclusive flag — the DSP chain doesn't change
    /// how the device is driven, and the engine reads this to decide native-rate
    /// switching, so it must reflect the real (inner) sink, not the trait default.
    fn exclusive(&self) -> bool {
        self.inner.exclusive()
    }

    /// Reopen the wrapped device at `rate` (native-rate switching), then re-set up the
    /// DSP chain for the device's NEW rate: the EQ filters are designed per sample rate
    /// and the analyzer's bin→frequency map is rate-dependent, so a stale rate would
    /// mis-design both. The filter bank is rebuilt fresh (no stale state across the
    /// rate change) on the next block.
    fn reopen(&mut self, rate: u32) -> Result<(), SinkError> {
        self.inner.reopen(rate)?;
        let info = self.inner.stream_info();
        if info.sample_rate != self.rate {
            self.rate = info.sample_rate;
            self.channels = usize::from(info.channels.max(1));
            // Force `sync()` to redesign every unit for the new rate (clearing the old
            // filter bank so no stale history carries across the rate change).
            self.units.clear();
            self.preamps.clear();
            self.applied_gen = None;
            self.analyzer = Analyzer::new(self.rate as f32, self.spectrum.clone());
        }
        Ok(())
    }

    fn write(&mut self, frames: &[i32]) -> Result<(), SinkError> {
        // Feed the analyzer the pre-chain signal (the track's own content) whether
        // or not the chain is engaged — the UI overlays the response to show it.
        self.analyzer.feed(frames, self.channels);
        if !self.sync() {
            // Empty/flat/off → bit-exact passthrough (no i32↔f32 round-trip).
            return self.inner.write(frames);
        }
        self.buf_f32.clear();
        self.buf_f32.extend(frames.iter().map(|&x| x as f32));
        // Apply each unit in order: its preamp, then its band cascade.
        for (eq, &preamp) in self.units.iter_mut().zip(&self.preamps) {
            if (preamp - 1.0).abs() > 1e-9 {
                for x in &mut self.buf_f32 {
                    *x *= preamp;
                }
            }
            eq.process(&mut self.buf_f32, self.channels);
        }
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
        // A seek is a discontinuity — drop every unit's filter tail so it can't ring
        // into the new position.
        for eq in &mut self.units {
            eq.reset();
        }
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
        fn reopen(&mut self, rate: u32) -> Result<(), SinkError> {
            // Model an in-place device reopen at a new rate (what `DspSink::reopen`
            // forwards to) and forget prior output so a test can see only post-reopen.
            self.info.sample_rate = rate;
            self.out.clear();
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

    /// Build a [`DspSink`] over `inner` with a throwaway analyzer buffer (the tests
    /// assert on the audio path, not the spectrum).
    fn dsp_sink(inner: Capture, shared: DspShared) -> DspSink<Capture> {
        DspSink::new(
            inner,
            shared,
            Arc::new(Mutex::new(SpectrumFrame::default())),
        )
    }

    /// A one-unit chain (the EQ unit enabled); `edit` tweaks that unit.
    fn chain_with(edit: impl FnOnce(&mut EqUnit)) -> DspShared {
        let mut unit = EqUnit::default();
        edit(&mut unit);
        Arc::new(Mutex::new(DspChain {
            units: vec![DspUnit::Eq(unit)],
            generation: 1,
        }))
    }

    // ---- DspSink behaviour ----------------------------------------------------

    #[test]
    fn dsp_sink_forwards_exclusive_from_the_wrapped_sink() {
        // Regression: the engine reads `Sink::exclusive()` THROUGH this DSP wrapper to
        // decide native-rate switching. If `DspSink` used the trait default (`false`),
        // the whole bit-perfect / exclusive-output feature would be silently inert.
        struct Inner(bool);
        impl Sink for Inner {
            fn stream_info(&self) -> StreamInfo {
                info(48_000, 2)
            }
            fn exclusive(&self) -> bool {
                self.0
            }
            fn write(&mut self, _: &[i32]) -> Result<(), SinkError> {
                Ok(())
            }
            fn flush(&mut self) -> Result<(), SinkError> {
                Ok(())
            }
        }
        let spectrum = || Arc::new(Mutex::new(SpectrumFrame::default()));
        let exclusive = DspSink::new(
            Inner(true),
            Arc::new(Mutex::new(DspChain::default())),
            spectrum(),
        );
        assert!(
            exclusive.exclusive(),
            "an exclusive inner sink reports exclusive through the wrapper"
        );
        let shared = DspSink::new(
            Inner(false),
            Arc::new(Mutex::new(DspChain::default())),
            spectrum(),
        );
        assert!(
            !shared.exclusive(),
            "a shared inner sink reports shared through the wrapper"
        );
    }

    #[test]
    fn a_dsp_unit_eq_applies_the_same_gain_as_the_bare_eq_unit() {
        // The typed-unit wrapper must be behaviour-identical to the old EQ-only
        // chain: a `DspUnit::Eq` built from the same `EqUnit` boosts its band by the
        // same gain. Guards the enum generalization against a regression.
        let n = 24_000; // 0.5 s
        let amp = 3_000_000.0; // ~0.36 FS — headroom for a +6 dB boost
        let mut unit = EqUnit::default();
        unit.bands[3] = bell(1000.0, 6.0, 1.0);
        let shared = Arc::new(Mutex::new(DspChain {
            units: vec![DspUnit::Eq(unit)],
            generation: 1,
        }));
        let mut sink = dsp_sink(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );
        let input = sine(1000.0, FS, n, amp);
        let in_peak = tail_peak(&input);
        sink.write(&input).unwrap();
        let gain = tail_peak(&sink.inner.out) / in_peak;
        let want = db_to_lin(6.0); // ≈1.995×
        assert!(
            (gain / want - 1.0).abs() < 0.08,
            "a DspUnit::Eq +6 dB band should ~double the 1 kHz amplitude: gain {gain:.3}, want {want:.3}"
        );
    }

    #[test]
    fn dsp_sink_reopen_redesigns_the_eq_for_the_new_rate() {
        // After a native-rate reopen, the EQ must be RE-designed for the device's NEW
        // rate. A bell at 1 kHz boosts a 1 kHz tone at 48k; after reopening at 96k, a
        // 1 kHz tone (generated at 96k) must STILL be boosted — if the filter kept its
        // 48k coefficients, that tone would land at the wrong normalized frequency and
        // NOT be boosted (the staleness bug this guards against).
        let n = 24_000;
        let amp = 3_000_000.0;
        let mut unit = EqUnit::default();
        unit.bands[3] = bell(1000.0, 6.0, 1.0);
        let shared = Arc::new(Mutex::new(DspChain {
            units: vec![DspUnit::Eq(unit)],
            generation: 1,
        }));
        let mut sink = dsp_sink(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );

        // At 48k: the 1 kHz tone is boosted ~+6 dB.
        let in48 = sine(1000.0, 48_000.0, n, amp);
        let in_peak = tail_peak(&in48);
        sink.write(&in48).unwrap();
        let gain48 = tail_peak(&sink.inner.out) / in_peak;
        assert!(
            (gain48 / db_to_lin(6.0) - 1.0).abs() < 0.1,
            "1 kHz boosted at 48k: gain {gain48:.3}"
        );

        // Reopen the device at 96k (Capture::reopen changes its rate + clears output).
        sink.reopen(96_000).unwrap();
        assert_eq!(
            sink.stream_info().sample_rate,
            96_000,
            "reopen forwards the new device rate"
        );

        // At 96k: a 1 kHz tone (generated at 96k) is STILL boosted — EQ redesigned.
        let in96 = sine(1000.0, 96_000.0, n, amp);
        let in_peak96 = tail_peak(&in96);
        sink.write(&in96).unwrap();
        let gain96 = tail_peak(&sink.inner.out) / in_peak96;
        assert!(
            (gain96 / db_to_lin(6.0) - 1.0).abs() < 0.15,
            "the 1 kHz band must follow the rate change (redesigned for 96k): gain {gain96:.3}"
        );
    }

    #[test]
    fn disabled_or_flat_is_a_bit_exact_passthrough() {
        let input = sine(1000.0, FS, 4096, 3_000_000.0);
        // Disabled.
        let off = Arc::new(Mutex::new(DspChain::default()));
        let mut sink = dsp_sink(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            off,
        );
        sink.write(&input).unwrap();
        assert_eq!(sink.inner.out, input, "a disabled EQ is bit-exact");
        // Enabled but all-flat (default bands, 0 dB everywhere).
        let flat = chain_with(|_| {});
        let mut sink = dsp_sink(
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
        let shared = chain_with(|c| {
            c.bands[3] = bell(1000.0, 6.0, 1.0);
        });
        let mut sink = dsp_sink(
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
        let shared = chain_with(|c| c.bands[3] = bell(1000.0, 10.0, 2.0));
        let mut sink = dsp_sink(
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
        let shared = chain_with(|c| c.bands[3] = bell(1000.0, 6.0, 1.0));
        let mut sink = dsp_sink(
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
        let shared = Arc::new(Mutex::new(DspChain::default())); // starts off
        let mut sink = dsp_sink(
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
            let mut unit = EqUnit::default();
            unit.bands[3] = bell(1000.0, 6.0, 1.0);
            c.units = vec![DspUnit::Eq(unit)];
            c.generation += 1;
        }
        sink.inner.out.clear();
        sink.write(&input).unwrap();
        assert!(
            tail_peak(&sink.inner.out) > flat_peak * 1.5,
            "a live generation bump must take effect on the next block"
        );
    }

    #[test]
    fn two_units_apply_in_series() {
        let n = 24_000;
        let amp = 2_000_000.0; // ~0.24 FS — headroom for +6 dB total
        // Two units, each a +3 dB bell at 1 kHz → +6 dB overall (cascade adds in dB).
        let a = {
            let mut u = EqUnit::default();
            u.bands[3] = bell(1000.0, 3.0, 1.0);
            u
        };
        let b = {
            let mut u = EqUnit::default();
            u.bands[3] = bell(1000.0, 3.0, 1.0);
            u
        };
        let shared = Arc::new(Mutex::new(DspChain {
            units: vec![DspUnit::Eq(a), DspUnit::Eq(b)],
            generation: 1,
        }));
        let mut sink = dsp_sink(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );
        let input = sine(1000.0, FS, n, amp);
        let in_peak = tail_peak(&input);
        sink.write(&input).unwrap();
        let gain = tail_peak(&sink.inner.out) / in_peak;
        let want = db_to_lin(6.0); // ≈1.995×
        assert!(
            (gain / want - 1.0).abs() < 0.1,
            "two +3 dB units in series should give ~+6 dB: gain {gain:.3}, want {want:.3}"
        );
    }

    #[test]
    fn a_disabled_unit_in_the_chain_is_a_passthrough() {
        let n = 12_000;
        let amp = 3_000_000.0;
        // One enabled +6 dB bell, then a disabled unit with a big boost that must
        // NOT apply.
        let on = {
            let mut u = EqUnit::default();
            u.bands[3] = bell(1000.0, 6.0, 1.0);
            u
        };
        let off = {
            let mut u = EqUnit::default();
            u.bands[3] = bell(1000.0, 12.0, 1.0);
            u.enabled = false;
            u
        };
        let shared = Arc::new(Mutex::new(DspChain {
            units: vec![DspUnit::Eq(on), DspUnit::Eq(off)],
            generation: 1,
        }));
        let mut sink = dsp_sink(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );
        let input = sine(1000.0, FS, n, amp);
        let gain = {
            let in_peak = tail_peak(&input);
            sink.write(&input).unwrap();
            tail_peak(&sink.inner.out) / in_peak
        };
        let want = db_to_lin(6.0); // only the enabled +6 dB unit applies
        assert!(
            (gain / want - 1.0).abs() < 0.1,
            "the disabled unit must not add gain: gain {gain:.3}, want {want:.3}"
        );
    }

    // ---- End-to-end: EQ applied to RESAMPLED audio (the previously untested seam) --

    use soul_audio_core::{Deck, DecodeError, Decoder, Progress, Quality, SeekableDecoder};

    /// A synthetic interleaved sine source at `rate` Hz, 24-bit — so a real `Deck` can
    /// decode → resample → (EQ) exactly as it does for a 44.1 kHz file into a 48 kHz
    /// device.
    struct SineDecoder {
        rate: u32,
        ch: usize,
        freq: f32,
        amp: f32,
        frames: u64,
        pos: u64,
    }
    impl Decoder for SineDecoder {
        fn stream_info(&self) -> StreamInfo {
            StreamInfo {
                sample_rate: self.rate,
                channels: self.ch as u16,
                format: SampleFormat::Int { bits: 24 },
            }
        }
        fn decode_into(&mut self, out: &mut [i32]) -> Result<usize, DecodeError> {
            let want = ((out.len() / self.ch) as u64).min(self.frames - self.pos) as usize;
            for f in 0..want {
                let t = (self.pos + f as u64) as f32 / self.rate as f32;
                let v = (self.amp * (2.0 * PI * self.freq * t).sin()).round() as i32;
                for c in 0..self.ch {
                    out[f * self.ch + c] = v;
                }
            }
            self.pos += want as u64;
            Ok(want * self.ch)
        }
    }
    impl SeekableDecoder for SineDecoder {
        fn seek_to(&mut self, sample: u64) -> Result<u64, DecodeError> {
            self.pos = sample.min(self.frames);
            Ok(self.pos)
        }
        fn total_frames(&self) -> Option<u64> {
            Some(self.frames)
        }
    }

    /// Goertzel magnitude of `freq` over one (deinterleaved) channel.
    fn goertzel(x: &[f32], freq: f32, fs: f32) -> f32 {
        let w = 2.0 * PI * freq / fs;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &v in x {
            let s0 = v + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2).sqrt()
    }

    /// The left channel of interleaved stereo i32, as f32.
    fn left(interleaved: &[i32]) -> Vec<f32> {
        interleaved.iter().step_by(2).map(|&x| x as f32).collect()
    }

    /// Render ~1 s of a 1 kHz tone at `src_rate` through a real resampling `Deck` into
    /// a 48 kHz `DspSink` carrying `chain`, returning the captured device output.
    fn render_resampled(src_rate: u32, amp: f32, chain: DspShared) -> Vec<i32> {
        let cap = Capture {
            info: info(48_000, 2),
            out: Vec::new(),
        };
        let mut deck: Deck<_, _, 1> = Deck::new(dsp_sink(cap, chain), Quality::Hq);
        deck.load(SineDecoder {
            rate: src_rate,
            ch: 2,
            freq: 1000.0,
            amp,
            frames: u64::from(src_rate),
            pos: 0,
        })
        .unwrap();
        deck.play();
        loop {
            match deck.pump().unwrap() {
                Progress::Ended | Progress::Idle => break,
                _ => {}
            }
        }
        deck.sink().inner.out.clone()
    }

    /// THE regression for the resample→EQ seam: a 44.1 kHz tone into a 48 kHz device
    /// WITH an active EQ band must come out at the right SPEED (frame count scales by
    /// 48000/44100), the right PITCH (still 1 kHz, not shifted), and boosted at the
    /// correct POST-resample frequency — never slower / distorted.
    #[test]
    fn eq_on_a_resampled_tone_keeps_speed_pitch_and_boosts_correctly() {
        let amp = CANONICAL_MAX * 0.45; // headroom for a +6 dB boost without clipping
        let flat = chain_with(|u| {
            u.enabled = true;
            u.bands = default_bands().to_vec();
        });
        let boosted = chain_with(|u| {
            u.enabled = true;
            u.bands = vec![bell(1000.0, 6.0, 1.0)];
        });

        let flat_out = render_resampled(44_100, amp, flat);
        let boost_out = render_resampled(44_100, amp, boosted);
        let fs = 48_000.0;

        // SPEED: 44.1k → 48k stretches the frame count by 48000/44100. The slack
        // (±1024, as in the engine's single_track_plays_once_and_ends) absorbs the HQ
        // resampler's latency tail + 256-frame block padding — a wrong SPEED (e.g. not
        // resampling at all → ~44100 frames, or ×2 → ~24000) is far outside it.
        let want_frames = 44_100f64 * 48_000.0 / 44_100.0;
        let got_frames = (flat_out.len() / 2) as f64;
        assert!(
            (got_frames - want_frames).abs() <= 1024.0,
            "resampled output is the wrong length (wrong speed): got {got_frames}, want ~{want_frames}"
        );

        // PITCH: 1 kHz dominates; the wrong-speed candidates stay far below it.
        let l = left(&flat_out);
        let at_1k = goertzel(&l, 1000.0, fs);
        for cand in [500.0, 2000.0, 1000.0 * 48.0 / 44.1, 1000.0 * 44.1 / 48.0] {
            let other = goertzel(&l, cand, fs);
            assert!(
                other < at_1k * 0.1,
                "wrong-speed pitch candidate {cand:.0} Hz too strong: {other:.0} vs 1k {at_1k:.0}"
            );
        }

        // BOOST hits the right POST-resample frequency: +6 dB ≈ ×2 at 1 kHz.
        let boost_1k = goertzel(&left(&boost_out), 1000.0, fs);
        let ratio = boost_1k / at_1k;
        assert!(
            (ratio / 2.0 - 1.0).abs() < 0.15,
            "the +6 dB band should ~double 1 kHz on resampled audio: ratio {ratio:.2}"
        );

        // No gross clipping at this headroom (the boosted peak stays in 24-bit range).
        let peak = boost_out.iter().map(|&x| x.unsigned_abs()).max().unwrap() as f32;
        assert!(
            peak <= CANONICAL_MAX + 1.0,
            "unexpected clip at safe headroom: peak {peak}"
        );
    }

    /// Documents the ONE real distortion source in the EQ path: a boost beyond
    /// headroom hard-clips at the 24-bit sink — independent of resampling. The user's
    /// saved unit carried a +7.2 dB preamp (×2.29), which clips hot audio. (Not a
    /// pipeline bug; this pins the behavior that motivates clip-headroom UX.)
    #[test]
    fn a_boosting_preamp_clips_hot_audio() {
        let hot = sine(1000.0, FS, 4_800, CANONICAL_MAX * 0.95);
        let shared = chain_with(|u| {
            u.enabled = true;
            u.preamp_db = 7.2;
        });
        let mut sink = dsp_sink(
            Capture {
                info: info(48_000, 1),
                out: Vec::new(),
            },
            shared,
        );
        sink.write(&hot).unwrap();
        let peak = sink
            .inner
            .out
            .iter()
            .map(|&x| x.unsigned_abs())
            .max()
            .unwrap() as f32;
        assert!(
            (peak - CANONICAL_MAX).abs() < 2.0,
            "a +7.2 dB preamp clips hot audio to full scale (peak {peak})"
        );
    }

    /// Reproduces the reported fault end-to-end through the REAL resampling `Deck`:
    /// the user's saved unit (a big +7.2 dB preamp + a modest band) clips a hot
    /// 44.1 kHz tone played into a 48 kHz device, while the SAME bands at 0 dB preamp
    /// stay within full scale. This pins "weird audio when EQ is applied" to the
    /// configured gain (clipping), not the resample→EQ pipeline — and proves dropping
    /// the preamp is the fix, without touching a live device.
    #[test]
    fn configured_gain_drives_clipping_through_the_resample_path() {
        let amp = CANONICAL_MAX * 0.7; // a realistically hot (~-3 dBFS) source

        // The user's unit: +7.2 dB preamp (×2.29) → pushes the signal past full scale.
        let user_cfg = chain_with(|u| {
            u.enabled = true;
            u.preamp_db = 7.2;
            u.bands = vec![bell(349.0, 2.1, 1.0)];
        });
        let clipped = render_resampled(44_100, amp, user_cfg);
        let clamp = CANONICAL_MAX as i32;
        let n_clipped = clipped.iter().filter(|&&x| x.abs() >= clamp).count();
        assert!(
            n_clipped > 100,
            "the +7.2 dB preamp must hard-clip the resampled signal (clipped samples: {n_clipped})"
        );

        // The same bands at 0 dB preamp are within headroom → no clipping (clean).
        let safe_cfg = chain_with(|u| {
            u.enabled = true;
            u.preamp_db = 0.0;
            u.bands = vec![bell(349.0, 2.1, 1.0)];
        });
        let clean = render_resampled(44_100, amp, safe_cfg);
        let peak = clean.iter().map(|&x| x.unsigned_abs()).max().unwrap() as f32;
        assert!(
            peak < CANONICAL_MAX,
            "at 0 dB preamp the resampled signal stays within full scale (peak {peak})"
        );
    }
}
