//! The EQ **spectrum analyzer**: a real-time FFT of the audio the [`EqSink`] sees,
//! published as log-frequency dB columns the UI draws behind the EQ curve (so you
//! see the playing track's frequency content, and, with the EQ response laid over
//! it, how the EQ reshapes it).
//!
//! It runs on the pump thread (inside [`EqSink::write`](crate::eq::EqSink)), not
//! the real-time device callback, so a windowed `realfft` per hop is safe. The
//! finished columns are published to a shared snapshot the UI polls at frame rate.
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// Log-frequency display columns the analyzer produces (20 Hz … 20 kHz).
pub const SPECTRUM_COLS: usize = 96;
/// FFT window size (≈85 ms at 48 kHz, the music sweet spot).
const FFT_SIZE: usize = 4096;
/// Hop between FFTs (75 % overlap).
const HOP: usize = 1024;
/// Display frequency range (Hz).
const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;
/// Magnitude floor (≈ −120 dBFS) so `log10` never sees zero.
const FLOOR_LIN: f32 = 1e-6;
/// The dB value a column rests at with no signal (the display floor).
pub const FLOOR_DB: f32 = -100.0;
/// Signed-24-bit full scale (the deck's canonical sample bound) → 0 dBFS.
const FULL_SCALE: f32 = 8_388_607.0;
/// Pink-ish tilt (+dB per octave, pivot 1 kHz) so typical music reads roughly flat.
const TILT_DB_PER_OCT: f32 = 3.0;
/// Per-FFT EMA coefficients: fast attack, slow release (snappy peaks, smooth decay).
const ATTACK: f32 = 0.5;
const RELEASE: f32 = 0.12;

/// A published spectrum snapshot: `cols[x]` is the smoothed dBFS level of display
/// column `x` (log-spaced `F_MIN`..`F_MAX`). `seq` bumps every FFT so the UI can
/// tell a fresh frame from a repeat.
#[derive(Debug, Clone)]
pub struct SpectrumFrame {
    /// Per-column smoothed dBFS levels on a log-spaced `F_MIN`..`F_MAX` axis.
    pub cols: [f32; SPECTRUM_COLS],
    /// Publish sequence counter: bumps every FFT so the UI can tell fresh from repeat.
    pub seq: u64,
}

impl Default for SpectrumFrame {
    fn default() -> Self {
        Self {
            cols: [FLOOR_DB; SPECTRUM_COLS],
            seq: 0,
        }
    }
}

/// A shared handle to the latest [`SpectrumFrame`]: the analyzer (pump thread)
/// writes it, the UI (frame loop) reads it.
pub type SpectrumShared = Arc<Mutex<SpectrumFrame>>;

/// A published live-loudness snapshot: the current momentary (400 ms) and short-term (3 s)
/// loudness in LUFS plus the recent sample peak in dBFS, all `f32::NEG_INFINITY` at silence.
/// `seq` bumps each publish so the UI can tell a fresh frame from a repeat. Fed by the
/// [`LiveLoudnessMeter`](soul_audio_loudness::LiveLoudnessMeter) in the `DspSink` from the
/// pre-chain signal (same tap as the spectrum), the real-time counterpart to the signal
/// path's whole-track loudness readouts.
#[derive(Debug, Clone, Copy)]
pub struct LiveLoudnessFrame {
    /// Momentary (400 ms window) loudness in LUFS; `NEG_INFINITY` at silence.
    pub momentary_lufs: f32,
    /// Short-term (3 s window) loudness in LUFS; `NEG_INFINITY` at silence.
    pub short_term_lufs: f32,
    /// Recent sample peak in dBFS; `NEG_INFINITY` at silence.
    pub peak_dbfs: f32,
    /// Publish sequence counter: bumps each frame so the UI can tell fresh from repeat.
    pub seq: u64,
}

impl Default for LiveLoudnessFrame {
    fn default() -> Self {
        Self {
            momentary_lufs: f32::NEG_INFINITY,
            short_term_lufs: f32::NEG_INFINITY,
            peak_dbfs: f32::NEG_INFINITY,
            seq: 0,
        }
    }
}

/// A shared handle to the latest [`LiveLoudnessFrame`]: the meter (pump thread) writes it,
/// the UI (frame loop) reads it.
pub type LiveLoudnessShared = Arc<Mutex<LiveLoudnessFrame>>;

/// How a display column reads the FFT bins: interpolate a fractional bin where the
/// column is narrower than one bin (the sparse low end), or take the max over a
/// span of bins where it covers several (the dense high end).
#[derive(Clone, Copy)]
enum ColMap {
    Interp { frac: f32 },
    Max { lo: usize, hi: usize },
}

/// The per-hop FFT analyzer. Owns reused `realfft` buffers and a sliding sample
/// ring, and publishes smoothed log-frequency columns.
pub struct Analyzer {
    fft: Arc<dyn realfft::RealToComplex<f32>>,
    window: Vec<f32>,
    win_norm: f32,
    inbuf: Vec<f32>,
    outbuf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    bin_db: Vec<f32>,
    ring: Vec<f32>,
    head: usize,
    filled: usize,
    since_fft: usize,
    col_map: Vec<ColMap>,
    col_tilt: Vec<f32>,
    smoothed: [f32; SPECTRUM_COLS],
    shared: SpectrumShared,
    seq: u64,
}

impl Analyzer {
    /// Build an analyzer for `fs` Hz publishing to `shared`.
    pub fn new(fs: f32, shared: SpectrumShared) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        // Periodic (DFT-even) Hann window: 0.5·(1 − cos(2πn/N)).
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|n| 0.5 * (1.0 - (2.0 * std::f32::consts::PI * n as f32 / FFT_SIZE as f32).cos()))
            .collect();
        let win_sum: f32 = window.iter().sum();
        let inbuf = fft.make_input_vec();
        let outbuf = fft.make_output_vec();
        let scratch = fft.make_scratch_vec();
        let bins = outbuf.len(); // N/2 + 1
        let (col_map, col_tilt) = Self::plan_columns(fs, bins);
        Self {
            fft,
            window,
            // One-sided amplitude normalisation: ×2 / Σw (Hann coherent gain).
            win_norm: 2.0 / win_sum,
            inbuf,
            outbuf,
            scratch,
            bin_db: vec![FLOOR_DB; bins],
            ring: vec![0.0; FFT_SIZE],
            head: 0,
            filled: 0,
            since_fft: 0,
            col_map,
            col_tilt,
            smoothed: [FLOOR_DB; SPECTRUM_COLS],
            shared,
            seq: 0,
        }
    }

    /// Precompute, for each display column, which FFT bins it reads and its tilt.
    fn plan_columns(fs: f32, bins: usize) -> (Vec<ColMap>, Vec<f32>) {
        let max_bin = bins - 1;
        let span = (SPECTRUM_COLS - 1) as f32;
        let bin_of = |f: f32| f * FFT_SIZE as f32 / fs;
        let freq_at = |t: f32| F_MIN * (F_MAX / F_MIN).powf(t);
        let mut map = Vec::with_capacity(SPECTRUM_COLS);
        let mut tilt = Vec::with_capacity(SPECTRUM_COLS);
        for x in 0..SPECTRUM_COLS {
            let t = x as f32 / span;
            let f_c = freq_at(t);
            let f_lo = freq_at(((x as f32) - 0.5) / span);
            let f_hi = freq_at(((x as f32) + 0.5) / span);
            let (b_lo, b_hi) = (bin_of(f_lo), bin_of(f_hi));
            let col = if b_hi - b_lo < 1.0 {
                ColMap::Interp {
                    frac: bin_of(f_c).clamp(0.0, max_bin as f32),
                }
            } else {
                ColMap::Max {
                    lo: (b_lo.floor() as usize).min(max_bin),
                    hi: (b_hi.ceil() as usize).min(max_bin),
                }
            };
            map.push(col);
            tilt.push(TILT_DB_PER_OCT * (f_c / 1000.0).log2());
        }
        (map, tilt)
    }

    /// Feed one block of interleaved `frames` (signed-24-bit canonical scale); runs
    /// an FFT whenever a hop's worth of fresh samples has arrived.
    pub fn feed(&mut self, frames: &[i32], channels: usize) {
        let channels = channels.max(1);
        for frame in frames.chunks(channels) {
            let sum: f32 = frame.iter().map(|&s| s as f32).sum();
            let mono = sum / (channels as f32 * FULL_SCALE);
            self.ring[self.head] = mono;
            self.head = (self.head + 1) % FFT_SIZE;
            self.filled = (self.filled + 1).min(FFT_SIZE);
            self.since_fft += 1;
            if self.filled == FFT_SIZE && self.since_fft >= HOP {
                self.since_fft = 0;
                self.run_fft();
            }
        }
    }

    fn run_fft(&mut self) {
        // Window the last FFT_SIZE samples, oldest → newest.
        for i in 0..FFT_SIZE {
            let s = self.ring[(self.head + i) % FFT_SIZE];
            self.inbuf[i] = s * self.window[i];
        }
        if self
            .fft
            .process_with_scratch(&mut self.inbuf, &mut self.outbuf, &mut self.scratch)
            .is_err()
        {
            return;
        }
        for (k, c) in self.outbuf.iter().enumerate() {
            let mag = c.norm() * self.win_norm;
            self.bin_db[k] = 20.0 * mag.max(FLOOR_LIN).log10();
        }
        for x in 0..SPECTRUM_COLS {
            let raw = match self.col_map[x] {
                ColMap::Interp { frac } => {
                    let lo = frac.floor() as usize;
                    let hi = (lo + 1).min(self.bin_db.len() - 1);
                    let f = frac - lo as f32;
                    self.bin_db[lo] * (1.0 - f) + self.bin_db[hi] * f
                }
                ColMap::Max { lo, hi } => self.bin_db[lo..=hi]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max),
            };
            let target = raw + self.col_tilt[x];
            let a = if target > self.smoothed[x] {
                ATTACK
            } else {
                RELEASE
            };
            self.smoothed[x] += a * (target - self.smoothed[x]);
        }
        self.seq += 1;
        if let Ok(mut frame) = self.shared.lock() {
            frame.cols.copy_from_slice(&self.smoothed);
            frame.seq = self.seq;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> SpectrumShared {
        Arc::new(Mutex::new(SpectrumFrame::default()))
    }

    /// The column nearest `freq` on the log axis.
    fn col_of(freq: f32) -> usize {
        let span = (SPECTRUM_COLS - 1) as f32;
        (((freq / F_MIN).ln() / (F_MAX / F_MIN).ln()) * span).round() as usize
    }

    /// A mono sine fed as interleaved-stereo i32 (24-bit scale).
    fn sine_block(freq: f32, fs: f32, n: usize, amp: f32) -> Vec<i32> {
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s =
                (amp * (2.0 * std::f32::consts::PI * freq * i as f32 / fs).sin()).round() as i32;
            v.push(s);
            v.push(s);
        }
        v
    }

    #[test]
    fn a_tone_peaks_in_its_own_column() {
        let fs = 48_000.0;
        let sh = shared();
        let mut a = Analyzer::new(fs, sh.clone());
        // ~1 s of a 1 kHz tone at ~0.5 FS.
        a.feed(&sine_block(1000.0, fs, fs as usize, FULL_SCALE * 0.5), 2);
        let frame = sh.lock().unwrap().clone();
        assert!(frame.seq > 0, "the analyzer ran at least one FFT");
        let peak_col = (0..SPECTRUM_COLS)
            .max_by(|&a, &b| frame.cols[a].total_cmp(&frame.cols[b]))
            .unwrap();
        let want = col_of(1000.0);
        assert!(
            (peak_col as i32 - want as i32).abs() <= 2,
            "the 1 kHz tone should peak near column {want}, got {peak_col}"
        );
        // Far away (100 Hz) is much quieter than at the tone.
        assert!(
            frame.cols[peak_col] > frame.cols[col_of(100.0)] + 20.0,
            "the tone should tower over an unrelated band"
        );
    }

    #[test]
    fn silence_stays_near_the_floor() {
        let fs = 48_000.0;
        let sh = shared();
        let mut a = Analyzer::new(fs, sh.clone());
        a.feed(&vec![0i32; fs as usize * 2], 2);
        let frame = sh.lock().unwrap().clone();
        assert!(
            frame.cols.iter().all(|&db| db < FLOOR_DB + 10.0),
            "silence should sit near the display floor"
        );
    }

    #[test]
    fn a_higher_tone_peaks_in_a_higher_column() {
        let fs = 48_000.0;
        let sh = shared();
        let mut a = Analyzer::new(fs, sh.clone());
        a.feed(&sine_block(5000.0, fs, fs as usize, FULL_SCALE * 0.5), 2);
        let frame = sh.lock().unwrap().clone();
        let peak = (0..SPECTRUM_COLS)
            .max_by(|&a, &b| frame.cols[a].total_cmp(&frame.cols[b]))
            .unwrap();
        assert!(
            peak > col_of(1000.0),
            "a 5 kHz tone peaks to the right of 1 kHz ({peak} > {})",
            col_of(1000.0)
        );
    }
}
