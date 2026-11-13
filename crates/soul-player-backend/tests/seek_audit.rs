//! END-TO-END **seek-audit** harness — the programmatic replacement for "trust
//! the user's ears" on seek quality, as a **committed, CI-safe, fixturized**
//! matrix.
//!
//! It drives a real [`ServicePlayer`] (the backend's own
//! `apply_command`/`poll_open`/`provide`/`pump` driver loop — the FULL app path,
//! synchronously, no actor thread, no audio device) over the committed
//! `testdata/tone.*` fixtures, through the real desktop decoder
//! ([`open_track`] → `OwnedDecoder`) → [`Deck`] → a faithful **[`CaptureSink`]**
//! that reproduces what the user actually hears at a seek:
//!
//! * the sink applies the output-stage volume gain like `CpalSink`;
//! * on `discard()` (a seek) it ramps the last ~8 ms of already-captured audio
//!   down to zero with the **same falling smoothstep** the real
//!   `soul-audio-desktop::sink::CpalSink` (its private `Fill`) uses — verified
//!   against `b7f0489`: `DISCARD_FADE_MS = 8`, `g = t·t·(3 − 2t)`,
//!   `cur_gain += (target − cur_gain)·0.01`. The deck applies the rising
//!   fade-IN on the new side (`deck.rs` `FADE_MS = 8`, same curve), so the
//!   captured stream across a seek is exactly
//!   `old … fade-out → 0 … 0 ← fade-in … new`.
//!
//! The fixtures are the canonical `soul-audio` test tone — left = 440 Hz, right =
//! 659.25 Hz, −6 dB, derived from one master (`testdata/generate.sh`), so a
//! format's post-seek audio cross-checks against an independent decode of the
//! SAME file. A single 440 Hz left channel (period ~100 samples at 44.1 kHz) is
//! why the ±64-lag cross-correlation on channel 0 has a unique peak at lag 0.
//!
//! For each `(format × two seek targets ≈ 30 %/70 %)` it asserts:
//!   1. **click-free** — the splice window's second-difference peak stays within
//!      `K_D2`× the surrounding (un-faded) audio's, two-sided (pre AND post
//!      seek), and introduces no hard step;
//!   2. **seek-landed-correct** — normalized cross-correlation of the captured
//!      post-seek audio vs an INDEPENDENT `open_track`+`seek_to` decode at the
//!      requested frame peaks at lag 0 and clears the row's floor (lossless
//!      ~0.99, lossy looser);
//!   3. **duration** — `project_tick().duration_secs` is `Some` and ≈ the row's
//!      known length (ADTS `.aac` / MP1 / MP2 now report an exact length);
//!   4. **plays** — the pre-seek audio is non-silent with a sane peak/RMS.
//!
//! Everything runs deterministically on a 64 MiB-stack thread (no device, no
//! second thread beyond that, no ffmpeg at test time, `CARGO_MANIFEST_DIR`-
//! relative paths). Set `E2E_WRITE_CLIPS=1` to also drop a ≈2 s WAV per splice
//! under `target/e2e-seek-clips/` for a human to audition — off by default so CI
//! stays silent and fast.

use std::fs;
use std::path::{Path, PathBuf};

use soul_audio_control::Player;
use soul_audio_core::{
    Deck, DecodeError, Decoder, Quality, SampleFormat, SeekableDecoder, Sink, SinkError, StreamInfo,
};
use soul_audio_desktop::{OwnedDecoder, open_track};
use soul_audio_engine::Engine;

use soul_player_backend::driver::queue_for;
use soul_player_backend::{Command, ServicePlayer, apply_command, project_tick};

// --------------------------------------------------------------------------
// The declarative fixture matrix (mirrors core `tests/matrix.rs`): one row per
// format carries the only per-format data; the generic runner reads the row.
// --------------------------------------------------------------------------

/// One format under audit. All derive from the same master tone, so `rate`/`ch`
/// and `known_secs` are known a-priori and asserted.
struct Fixture {
    /// File under `testdata/`.
    file: &'static str,
    /// Lossy decoders get a looser cross-correlation floor.
    lossy: bool,
    /// Expected native sample rate (the sink is built to match → deck takes its
    /// no-resampler pass-through, so captured audio == the normalized source).
    rate: u32,
    /// Expected channel count.
    ch: u16,
    /// Known content length (s); `duration_secs` must be `Some` within `DUR_TOL`.
    known_secs: f64,
    /// Cross-correlation floor the post-seek audio must clear.
    corr_floor: f64,
}

/// The audited formats. ADTS AAC, AAC-LC, ALAC, MP1, MP2, Opus and Vorbis sit
/// beside the WAV/FLAC/MP3 baseline — the seek-relevant format spread. MP1 is the
/// committed Layer-I conformance vector concatenated to a seekable length (ffmpeg
/// has no Layer-I encoder); Opus is 48 kHz and MP1 32 kHz, so the harness also
/// exercises non-44.1 kHz native rates.
#[rustfmt::skip]
const FIXTURES: &[Fixture] = &[
    Fixture { file: "tone.wav",        lossy: false, rate: 44100, ch: 2, known_secs: 2.000, corr_floor: 0.99 },
    Fixture { file: "tone.flac",       lossy: false, rate: 44100, ch: 2, known_secs: 2.000, corr_floor: 0.99 },
    Fixture { file: "tone.m4a",        lossy: false, rate: 44100, ch: 2, known_secs: 2.000, corr_floor: 0.99 }, // ALAC
    Fixture { file: "tone.mp3",        lossy: true,  rate: 44100, ch: 2, known_secs: 2.000, corr_floor: 0.90 },
    Fixture { file: "tone.aac",        lossy: true,  rate: 44100, ch: 2, known_secs: 2.077, corr_floor: 0.90 }, // ADTS
    Fixture { file: "tone-aac.m4a",    lossy: true,  rate: 44100, ch: 2, known_secs: 2.000, corr_floor: 0.90 }, // AAC-LC
    Fixture { file: "tone.mp2",        lossy: true,  rate: 44100, ch: 2, known_secs: 2.011, corr_floor: 0.90 },
    Fixture { file: "tone-vorbis.ogg", lossy: true,  rate: 44100, ch: 2, known_secs: 2.000, corr_floor: 0.90 },
    Fixture { file: "tone.opus",       lossy: true,  rate: 48000, ch: 2, known_secs: 2.006, corr_floor: 0.90 },
    Fixture { file: "tone.mp1",        lossy: true,  rate: 32000, ch: 2, known_secs: 2.340, corr_floor: 0.90 }, // Layer I
];

/// Seek targets as a fraction of the row's known length (≈30 % / ≈70 %).
const SEEK_FRACS: &[f64] = &[0.30, 0.70];

// --------------------------------------------------------------------------
// Knobs (ported from the local real-file harness, which tuned them against real
// music; unchanged — the fixtures are tonal but the thresholds are only looser).
// --------------------------------------------------------------------------

/// Duration tolerance vs `known_secs` (covers per-encoder trim/padding).
const DUR_TOL_SECS: f64 = 0.10;
/// Splice-window d2 peak must stay within `K_D2`× the clean window's d2 peak.
const K_D2: i64 = 4;
/// Splice's largest adjacent step must stay within `K_STEP`× the clean step.
const K_STEP: i64 = 4;
/// A true hard clear steps ≈ full scale (2^23); the splice must stay well under.
const HARD_STEP_CEIL: i64 = 1 << 21;
/// Non-silence floor and clip ceiling for the "plays" check (canonical 24-bit).
const PLAYS_PEAK_FLOOR: i64 = 50_000;
const PLAYS_PEAK_CEIL: i64 = 1 << 24;
const PLAYS_RMS_FLOOR: f64 = 10_000.0;

/// Cross-correlation length (frames) and the ± lag range searched (frames).
const CORR_FRAMES: usize = 4096;
const LAGS: usize = 64;
/// d2 window half-width (frames) on each side of the splice.
const SPLICE_HALF: usize = 1024;
/// Clean-window width and its gap from the fade region (frames).
const CLEAN_WIN: usize = 2048;
const CLEAN_MARGIN: usize = 512;

/// Per-channel frames rendered before the first seek, and after each seek. Kept
/// well above the window math below yet comfortably inside the ~2 s fixtures
/// (even a 70 % seek leaves room before the master's 50 ms out-fade).
const PRE_SEEK_SECS: f64 = 0.5;
const POST_SEEK_SECS: f64 = 0.4;

// --------------------------------------------------------------------------
// A faithful capturing sink (mirrors `CpalSink`'s seek behavior on b7f0489).
// --------------------------------------------------------------------------

/// Captures every sample the deck writes (interleaved canonical 24-bit, as the
/// deck emits) and reproduces the real sink's seek audio: an output-stage volume
/// gain on `write`, and a falling-smoothstep fade-OUT of the last `fade_len`
/// captured samples on `discard` (seek), recording the splice boundary.
struct CaptureSink {
    rate: u32,
    channels: u16,
    /// Fade-out length in **interleaved** samples: `round(8 ms · rate · ch /
    /// 1000)` — the same 8 ms window as the deck's fade-IN (`deck.rs` `FADE_MS`)
    /// and `CpalSink`'s `DISCARD_FADE_MS` fade-out.
    fade_len: u32,
    /// Smoothed output gain, ramped per-sample toward `target_gain` exactly like
    /// `CpalSink::Fill` (so a volume change is click-free). Held at 1.0 here.
    cur_gain: f32,
    target_gain: f32,
    written: Vec<i32>,
    /// Interleaved index in `written` where each post-seek segment begins (the
    /// sample right after the faded-out tail).
    splices: Vec<usize>,
}

impl CaptureSink {
    fn new(rate: u32, channels: u16) -> Self {
        // round(8 ms · rate · ch / 1000) in u64 so no rate/channel pair overflows.
        let fade_len = ((8u64 * u64::from(rate) * u64::from(channels) + 500) / 1000) as u32;
        Self {
            rate,
            channels,
            fade_len,
            cur_gain: 1.0,
            target_gain: 1.0,
            written: Vec::new(),
            splices: Vec::new(),
        }
    }
}

impl Sink for CaptureSink {
    fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            sample_rate: self.rate,
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }

    fn write(&mut self, frames: &[i32]) -> Result<(), SinkError> {
        // Output-stage gain, ramped per interleaved sample toward the target —
        // the mirror of `CpalSink::Fill::next` (truncating cast, not rounding).
        for &s in frames {
            self.cur_gain += (self.target_gain - self.cur_gain) * 0.01;
            self.written.push((s as f32 * self.cur_gain) as i32);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }

    fn discard(&mut self) -> Result<(), SinkError> {
        // Seek: ramp the last `fade_len` already-captured interleaved samples
        // down to zero with the falling smoothstep `g = t·t·(3 − 2t)`,
        // `t = fade_remaining / fade_len` running 1 → 0 across the window (the
        // exact mirror of the deck's rising fade-in and of `CpalSink`'s fade-out:
        // the earliest windowed sample is full level and continuous with the
        // prior audio, descending to ≈0 at the splice). Then mark the boundary.
        let n = self.written.len();
        let l = (self.fade_len as usize).min(n);
        let start = n - l;
        let denom = self.fade_len as f32;
        for i in 0..l {
            let fade_remaining = (l - i) as f32; // earliest → fade_len, latest → 1
            let t = (fade_remaining / denom).min(1.0);
            let g = t * t * (3.0 - 2.0 * t);
            let v = self.written[start + i];
            self.written[start + i] = (v as f32 * g) as i32;
        }
        self.splices.push(n);
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        self.target_gain = volume.clamp(0.0, 1.0);
    }
}

// --------------------------------------------------------------------------
// Metric helpers (mirror soul-audio-core's tests/declick.rs).
// --------------------------------------------------------------------------

/// `max |x[i] − 2·x[i−1] + x[i−2]|` — the discrete second difference's peak. A
/// smooth (incl. smoothstep-faded) signal keeps this bounded; a slope break (a
/// hard splice/click) spikes it toward full scale.
fn second_difference_peak(xs: &[i32]) -> i64 {
    xs.windows(3)
        .map(|w| (i64::from(w[2]) - 2 * i64::from(w[1]) + i64::from(w[0])).abs())
        .max()
        .unwrap_or(0)
}

/// `max |x[i] − x[i−1]|` — the largest adjacent step (a hard clear ≈ full scale).
fn max_abs_step(xs: &[i32]) -> i64 {
    xs.windows(2)
        .map(|w| i64::from(w[1]).abs_diff(i64::from(w[0])) as i64)
        .max()
        .unwrap_or(0)
}

/// Deinterleave channel `c` of an interleaved `ch`-channel buffer.
fn channel(xs: &[i32], ch: usize, c: usize) -> Vec<i32> {
    xs.chunks_exact(ch).map(|f| f[c]).collect()
}

/// Peak-abs and RMS of an interleaved buffer (over all channels), canonical 24-bit.
fn peak_and_rms(xs: &[i32]) -> (i64, f64) {
    if xs.is_empty() {
        return (0, 0.0);
    }
    let mut peak = 0i64;
    let mut sumsq = 0.0f64;
    for &s in xs {
        let a = i64::from(s).abs();
        if a > peak {
            peak = a;
        }
        sumsq += f64::from(s) * f64::from(s);
    }
    (peak, (sumsq / xs.len() as f64).sqrt())
}

/// Pearson correlation of two equal-length series (zero-mean, unit-energy
/// normalized — so it is invariant to the 24-bit-normalization scale factor
/// between captured and reference audio). 0 on a degenerate (flat) input.
fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let ma = a.iter().sum::<f64>() / n;
    let mb = b.iter().sum::<f64>() / n;
    let (mut num, mut da, mut db) = (0.0f64, 0.0f64, 0.0f64);
    for (&x, &y) in a.iter().zip(b) {
        let (xa, yb) = (x - ma, y - mb);
        num += xa * yb;
        da += xa * xa;
        db += yb * yb;
    }
    if da <= 0.0 || db <= 0.0 {
        return 0.0;
    }
    num / (da.sqrt() * db.sqrt())
}

/// Normalized cross-correlation of `a` (length N) against `b_window` (length
/// N + 2·LAGS), searching lag ∈ [−LAGS, +LAGS]. Returns `(best_lag, peak)`.
fn best_cross_correlation(a: &[f64], b_window: &[f64]) -> (i64, f64) {
    let n = a.len();
    let mut best = (0i64, f64::NEG_INFINITY);
    for d in 0..=(2 * LAGS) {
        let corr = pearson(a, &b_window[d..d + n]);
        if corr > best.1 {
            best = (d as i64 - LAGS as i64, corr);
        }
    }
    best
}

fn to_f64(xs: &[i32]) -> Vec<f64> {
    xs.iter().map(|&x| f64::from(x)).collect()
}

// --------------------------------------------------------------------------
// Minimal 24-bit PCM WAV writer (no `hound` dependency in this workspace).
// --------------------------------------------------------------------------

/// Write interleaved canonical-24-bit samples as a 24-bit PCM WAV. Returns the
/// absolute path written.
fn write_wav_24(path: &Path, samples: &[i32], rate: u32, channels: u16) -> PathBuf {
    const BYTES_PER_SAMPLE: u32 = 3;
    let data_len = samples.len() as u32 * BYTES_PER_SAMPLE;
    let byte_rate = rate * u32::from(channels) * BYTES_PER_SAMPLE;
    let block_align = (channels as u32 * BYTES_PER_SAMPLE) as u16;

    let mut buf = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&24u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = s.clamp(-(1 << 23), (1 << 23) - 1);
        let le = v.to_le_bytes(); // low 3 bytes are the 24-bit two's-complement value
        buf.extend_from_slice(&le[..3]);
    }
    fs::write(path, &buf).expect("write wav");
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// --------------------------------------------------------------------------
// Driving the real ServicePlayer path.
// --------------------------------------------------------------------------

type HarnessPlayer = ServicePlayer<CaptureSink, OwnedDecoder>;

fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// Service every open the player requests by opening the one real file, then
/// pump until the sink has captured `frames` more interleaved frames (bounded).
fn pump_capture_frames(p: &mut HarnessPlayer, path: &Path, frames: usize, ch: usize) {
    let target = p.deck().sink().written.len() + frames * ch;
    for _ in 0..1_000_000 {
        while let Some(id) = p.poll_open() {
            let dec = open_track(path).expect("open_track for provide");
            p.provide(id, dec).expect("provide decoder");
        }
        p.pump().expect("pump");
        if p.deck().sink().written.len() >= target {
            break;
        }
    }
}

/// An independent reference decode: open the file afresh, seek to `frame`, and
/// decode `want` frames of channel `c` (source scale). Proves the harness's
/// end-to-end seek landed where a clean decode at that timestamp sits.
fn reference_channel(path: &Path, frame: u64, want: usize, ch: usize, c: usize) -> Vec<i32> {
    let mut dec = open_track(path).expect("open_track reference");
    dec.seek_to(frame).expect("reference seek");
    let mut out: Vec<i32> = Vec::with_capacity(want * ch);
    let mut scratch = vec![0i32; 4096 * ch];
    while out.len() < want * ch {
        let n = match dec.decode_into(&mut scratch) {
            Ok(0) => break,
            Ok(n) => n,
            Err(DecodeError::Unsupported) => break,
            Err(e) => panic!("reference decode: {e:?}"),
        };
        out.extend_from_slice(&scratch[..n]);
    }
    channel(&out, ch, c)
}

/// Run `f` on a 64 MiB stack, returning its `Result` (or a panic turned into an
/// `Err`). 64 MiB matches `soul-audio-desktop`'s own `open_track` decode tests:
/// the owned decoder (~315 KiB `Pcm` by value) multiplies through many
/// debug-build move frames in the open_track→provide→deck→decode chain and
/// overflows a smaller stack on the Windows MSVC debug toolchain.
fn on_big_stack(f: impl FnOnce() -> Result<(), String> + Send) -> Result<(), String> {
    std::thread::scope(|s| {
        match std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn_scoped(s, f)
            .expect("spawn")
            .join()
        {
            Ok(r) => r,
            Err(_) => Err("harness thread panicked (see stderr)".into()),
        }
    })
}

/// `if cond {} else { return Err(format!(..)) }` — keeps the per-format runner
/// readable. The `if/else` form (not `if !cond`) avoids
/// `clippy::neg_cmp_op_on_partial_ord` on the f64 comparisons passed in.
macro_rules! require {
    ($cond:expr, $($arg:tt)*) => {
        if $cond {
        } else {
            return Err(format!($($arg)*));
        }
    };
}

// --------------------------------------------------------------------------
// The harness.
// --------------------------------------------------------------------------

#[test]
fn end_to_end_seek_audit() {
    let clips_dir = if std::env::var_os("E2E_WRITE_CLIPS").is_some() {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e-seek-clips");
        fs::create_dir_all(&d).expect("create clips dir");
        Some(d)
    } else {
        None
    };

    println!("\n=== END-TO-END SEEK AUDIT (ServicePlayer → Deck → CaptureSink) ===");
    let mut failures: Vec<String> = Vec::new();
    for fix in FIXTURES {
        let clips = clips_dir.clone();
        let res = on_big_stack(move || audit_one(fix, clips.as_deref()));
        if let Err(e) = res {
            failures.push(format!("{}: {e}", fix.file));
        }
    }
    println!(
        "=== seek audit complete: {}/{} fixtures passed ===\n",
        FIXTURES.len() - failures.len(),
        FIXTURES.len()
    );

    assert!(
        failures.is_empty(),
        "end-to-end seek-audit failures ({}):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

fn audit_one(fix: &Fixture, clips_dir: Option<&Path>) -> Result<(), String> {
    let path = testdata_dir().join(fix.file);
    require!(path.is_file(), "committed fixture {path:?} is missing");

    // Probe rate/channels so the sink is rate/channel-matched to the source and
    // the deck takes its no-resampler pass-through path (captured audio == the
    // normalized source, so the cross-correlation is a clean identity test).
    let si = open_track(&path)
        .map_err(|e| format!("open_track probe: {e}"))?
        .stream_info();
    let (rate, channels) = (si.sample_rate, si.channels);
    require!(
        rate == fix.rate && channels == fix.ch,
        "stream_info {rate} Hz/{channels}ch ≠ expected {} Hz/{}ch",
        fix.rate,
        fix.ch
    );
    let ch = usize::from(channels);
    let fade_frames = ((8u64 * u64::from(rate) + 500) / 1000) as usize;
    let pre_frames = (PRE_SEEK_SECS * f64::from(rate)) as usize;
    let post_frames = (POST_SEEK_SECS * f64::from(rate)) as usize;

    // Build a real ServicePlayer<CaptureSink, OwnedDecoder> and start playback
    // via the backend's own command driver — the full app path.
    let deck = Deck::new(CaptureSink::new(rate, channels), Quality::Hq);
    let engine = Engine::new(deck, queue_for(vec![0]));
    let mut player: HarnessPlayer = Player::new(engine, u64::from(rate));
    apply_command(
        &mut player,
        Command::PlayContext {
            ids: vec![0],
            start: 0,
        },
        0,
    )
    .map_err(|e| format!("PlayContext: {e:?}"))?;

    // Pre-seek audio.
    pump_capture_frames(&mut player, &path, pre_frames, ch);

    // (3) Duration — end-to-end, as the UI would see it. Every seekable stream
    // must report a real length so the seek bar works (incl. ADTS .aac / MP1 /
    // MP2, which now header-walk for an exact frame count).
    let dur = project_tick(&player, 1.0, false).duration_secs;
    let d = dur
        .ok_or_else(|| "deck reported no duration; a seekable stream must have one".to_string())?;
    require!(
        (d - fix.known_secs).abs() < DUR_TOL_SECS,
        "duration {d:.3}s not within {DUR_TOL_SECS}s of known {:.3}s",
        fix.known_secs
    );

    // (4) Plays — the pre-seek audio is non-silent with a sane peak/RMS.
    let (peak, rms) = peak_and_rms(&player.deck().sink().written);
    require!(
        (PLAYS_PEAK_FLOOR..=PLAYS_PEAK_CEIL).contains(&peak) && rms > PLAYS_RMS_FLOOR,
        "pre-seek audio peak={peak} rms={rms:.0} outside sane 24-bit range \
         (floor {PLAYS_PEAK_FLOOR}, ceil {PLAYS_PEAK_CEIL}, rms floor {PLAYS_RMS_FLOOR})"
    );

    // Seek targets (absolute seconds) and the frames they map to.
    let seeks: Vec<(f64, u64)> = SEEK_FRACS
        .iter()
        .map(|&f| {
            let secs = f * fix.known_secs;
            (secs, (secs.max(0.0) * f64::from(rate)) as u64)
        })
        .collect();

    // Record each seek's requested/landed frame as we render.
    let mut landed_frames: Vec<u64> = Vec::new();
    for &(secs, _frame) in &seeks {
        apply_command(&mut player, Command::Seek { secs }, 0)
            .map_err(|e| format!("seek@{secs:.3}s: {e:?}"))?;
        landed_frames.push(player.position_frames());
        pump_capture_frames(&mut player, &path, post_frames, ch);
    }

    // Pull the captured stream + splice boundaries out of the sink.
    let written = player.deck().sink().written.clone();
    let splices = player.deck().sink().splices.clone();
    require!(
        splices.len() == seeks.len(),
        "{} splices recorded, expected {}",
        splices.len(),
        seeks.len()
    );

    println!(
        "\n{}  rate={rate} ch={ch}  duration={d:.3}s  peak={peak} rms={rms:.0}",
        fix.file
    );

    for (k, &(secs, req)) in seeks.iter().enumerate() {
        let splice = splices[k]; // interleaved index
        let splice_frame = splice / ch;
        let landed = landed_frames[k];

        // ---- (1) click-free splice: d2 + step ratios, per channel ----
        let lo = splice.saturating_sub(SPLICE_HALF * ch);
        let hi = (splice + SPLICE_HALF * ch).min(written.len());
        let splice_win = &written[lo..hi];

        // Clean reference on BOTH sides, un-faded: the pre-seek audio just before
        // the fade-out AND the landed audio just after the fade-in settles. A
        // seek lands in audio whose dynamics may differ from the pre-seek side, so
        // the adaptive bound takes the max of both.
        let pre_hi = splice_frame.saturating_sub(fade_frames + CLEAN_MARGIN);
        let pre_lo = pre_hi.saturating_sub(CLEAN_WIN);
        let post_lo = splice_frame + fade_frames + CLEAN_MARGIN;
        let post_hi = (post_lo + CLEAN_WIN).min(written.len() / ch);
        require!(
            pre_hi > pre_lo && post_hi > post_lo,
            "seek@{secs:.3}s: clean windows degenerate (not enough rendered audio)"
        );
        let pre_win = &written[pre_lo * ch..pre_hi * ch];
        let post_win = &written[post_lo * ch..post_hi * ch];

        let mut clean_d2 = 0i64;
        let mut splice_d2 = 0i64;
        let mut clean_step = 0i64;
        let mut splice_step = 0i64;
        for c in 0..ch {
            clean_d2 = clean_d2
                .max(second_difference_peak(&channel(pre_win, ch, c)))
                .max(second_difference_peak(&channel(post_win, ch, c)));
            splice_d2 = splice_d2.max(second_difference_peak(&channel(splice_win, ch, c)));
            clean_step = clean_step
                .max(max_abs_step(&channel(pre_win, ch, c)))
                .max(max_abs_step(&channel(post_win, ch, c)));
            splice_step = splice_step.max(max_abs_step(&channel(splice_win, ch, c)));
        }

        // ---- (2) seek landed correctly: normalized cross-correlation ----
        let a_lo = (splice_frame + fade_frames) * ch;
        require!(
            a_lo + CORR_FRAMES * ch <= written.len(),
            "seek@{secs:.3}s: not enough post-seek audio for cross-correlation"
        );
        let a = to_f64(&channel(&written[a_lo..a_lo + CORR_FRAMES * ch], ch, 0));
        // Reference decoded from the SAME requested frame; windowed so lag 0
        // aligns a[i] with ref[fade_frames + i].
        let ref_ch0 = reference_channel(&path, req, fade_frames + CORR_FRAMES + 2 * LAGS, ch, 0);
        require!(
            ref_ch0.len() >= fade_frames - LAGS + CORR_FRAMES + 2 * LAGS,
            "seek@{secs:.3}s: reference decode too short ({} frames)",
            ref_ch0.len()
        );
        let b_start = fade_frames - LAGS;
        let b = to_f64(&ref_ch0[b_start..b_start + CORR_FRAMES + 2 * LAGS]);
        let (lag, corr) = best_cross_correlation(&a, &b);

        let d2_ratio = splice_d2 as f64 / clean_d2.max(1) as f64;
        println!(
            "  seek@{secs:>5.2}s  req={req} landed={landed} (Δ{})  \
             d2 splice={splice_d2} clean={clean_d2} ratio={d2_ratio:.2}  \
             step splice={splice_step} clean={clean_step}  xcorr peak={corr:.4}@lag{lag}",
            landed as i64 - req as i64,
        );

        // Absolute seek accuracy: a sample-accurate (lossless) decoder must land
        // EXACTLY on the requested frame. Lossy codecs land on a codec-frame /
        // granule boundary (and an Ogg stream's reported position is
        // page-granular), so they are held to account only by the audio-content
        // cross-correlation below — which compares against an independent decode
        // seeked the same way, so the whole end-to-end app path is still proven.
        if !fix.lossy {
            require!(
                landed == req,
                "seek@{secs:.3}s: lossless seek landed at frame {landed}, not the \
                 requested {req} — a sample-accurate seek must be exact"
            );
        }

        // The ratio test is only honest if the signal itself has real curvature.
        require!(
            clean_d2 > 100,
            "seek@{secs:.3}s: clean d2 {clean_d2} ~0 — window not musical enough"
        );
        require!(
            splice_d2 <= K_D2 * clean_d2,
            "seek@{secs:.3}s: splice d2 {splice_d2} exceeds {K_D2}× clean d2 {clean_d2} \
             — the seek introduced a slope break (click)"
        );
        require!(
            splice_step <= HARD_STEP_CEIL,
            "seek@{secs:.3}s: splice step {splice_step} exceeds hard-step ceiling \
             {HARD_STEP_CEIL} (≈2^23 is a hard clear) — the fade-out is broken"
        );
        require!(
            splice_step <= K_STEP * clean_step,
            "seek@{secs:.3}s: splice step {splice_step} exceeds {K_STEP}× clean step \
             {clean_step} — a hard step at the splice"
        );

        // Seek landed at the requested frame (lag 0) and decodes the same audio.
        require!(
            lag == 0,
            "seek@{secs:.3}s: cross-correlation peaks at lag {lag}, not 0 — the \
             end-to-end seek did not land at the requested timestamp"
        );
        require!(
            corr > fix.corr_floor,
            "seek@{secs:.3}s: cross-correlation peak {corr:.4} below floor {:.2} — \
             post-seek audio ≠ an independent decode at that frame",
            fix.corr_floor
        );

        // ---- opt-in audible artifact: ≈1 s before + ≈1 s after the splice ----
        if let Some(dir) = clips_dir {
            let one_sec = rate as usize;
            let w_lo = splice.saturating_sub(one_sec * ch);
            let w_hi = (splice + one_sec * ch).min(written.len());
            // Keep the extension (as `-ext`) so tone.wav/.flac/.m4a don't collide.
            let stem = fix.file.replace('.', "-");
            let clip = dir.join(format!("{stem}-seek-{secs:04.2}s.wav"));
            let abs = write_wav_24(&clip, &written[w_lo..w_hi], rate, channels);
            println!("            clip: {}", abs.display());
        }
    }

    Ok(())
}
