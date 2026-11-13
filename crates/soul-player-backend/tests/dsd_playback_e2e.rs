//! END-TO-END **DSD playback** audit — the programmatic replacement for "trust
//! the user's ears" on the recurring *"DSD plays at the wrong speed"* symptom.
//!
//! Drives a real [`ServicePlayer`] (the backend's own
//! `apply_command`/`poll_open`/`provide`/`pump` loop — the FULL app path,
//! synchronously, no actor thread, no audio device) over the committed
//! `testdata/tone.dsf` fixture (DSD64, A4 = 440 Hz on LEFT, E5 = 659.25 Hz on
//! RIGHT, ~1.0 s) through the real desktop decoder ([`open_track`] →
//! `OwnedDecoder::Dsf` → `PlaybackDecimateAdapter`) → [`Deck`] → a capturing sink.
//!
//! A wrong decimation ratio or a broken rate handshake would land the captured
//! tones *off* 440/659.25 Hz — i.e. stretched/compressed playback. We prove, via
//! a Goertzel power at the expected tone vs. the "wrong-speed" candidates (half,
//! double, and the 44.1↔48 kHz mismatch ratios), that each channel's energy sits
//! exactly where it should. Run at:
//!   * a **44.1 kHz** sink — the deck's no-resampler pass-through (DSD's native
//!     output rate for every family), and
//!   * a **48 kHz** sink — the deck resamples DSD's 44.1 kHz output, the real
//!     path for a 48 kHz device; pitch must survive the resample.
//!
//! Correctness only. Real-time *throughput* (the actual cause of the debug-build
//! "wrong speed") is guarded separately by soul-audio-core's
//! `dsd_decodes_faster_than_real_time`; a capture sink has no clock to starve.
//!
//! Runs on a 64 MiB stack (the owned decoder is large by value) with no device,
//! no second thread beyond that, and `CARGO_MANIFEST_DIR`-relative paths.

use std::path::PathBuf;

use soul_audio_control::Player;
use soul_audio_core::{
    Deck, Decoder, Quality, SampleFormat, SeekableDecoder, Sink, SinkError, StreamInfo,
};
use soul_audio_desktop::{OwnedDecoder, open_track};
use soul_audio_engine::Engine;

use soul_player_backend::driver::queue_for;
use soul_player_backend::{Command, ServicePlayer, apply_command, project_tick};

/// tone.dsf is DSD64 → 44.1 kHz, stereo, 22050 frames = 0.5 s.
const NATIVE_RATE: u32 = 44_100;
const KNOWN_SECS: f64 = 0.5;
const DUR_TOL_SECS: f64 = 0.1;
/// Render well UNDER the 0.5 s length so a live track still reports its duration
/// (over-pumping past EOF ends the track and `duration_frames` goes `None`), yet
/// leaves ≥0.3 s of steady audio for the Goertzel tone analysis.
const RENDER_SECS: f64 = 0.35;
/// Canonical 24-bit "plays" bounds.
const PLAYS_PEAK_FLOOR: i64 = 50_000;
const PLAYS_PEAK_CEIL: i64 = 1 << 24;
const PLAYS_RMS_FLOOR: f64 = 10_000.0;

/// A minimal capturing sink: records every interleaved sample the deck writes
/// (canonical 24-bit). No seeks in this test, so `discard` is a no-op.
struct CaptureSink {
    rate: u32,
    channels: u16,
    written: Vec<i32>,
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
        self.written.extend_from_slice(frames);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
    fn discard(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
    fn set_volume(&mut self, _volume: f32) {}
}

type HarnessPlayer = ServicePlayer<CaptureSink, OwnedDecoder>;

fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// Deinterleave channel `c` of an interleaved `ch`-channel buffer.
fn channel(xs: &[i32], ch: usize, c: usize) -> Vec<i32> {
    xs.chunks_exact(ch).map(|f| f[c]).collect()
}

fn peak_and_rms(xs: &[i32]) -> (i64, f64) {
    if xs.is_empty() {
        return (0, 0.0);
    }
    let mut peak = 0i64;
    let mut sumsq = 0.0f64;
    for &s in xs {
        peak = peak.max(i64::from(s).abs());
        sumsq += f64::from(s) * f64::from(s);
    }
    (peak, (sumsq / xs.len() as f64).sqrt())
}

/// Goertzel single-bin power of `samples` at `target_hz` (sampled at
/// `sample_rate`). Scale-invariant comparisons only (relative power).
fn goertzel_power(samples: &[i32], sample_rate: f64, target_hz: f64) -> f64 {
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    let w = 2.0 * std::f64::consts::PI * target_hz / sample_rate;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &x in samples {
        let s0 = f64::from(x) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (n as f64)
}

/// Service every open the player requests from the one real file, then pump until
/// the sink has captured at least `frames` interleaved frames (bounded).
fn pump_capture_frames(p: &mut HarnessPlayer, path: &std::path::Path, frames: usize, ch: usize) {
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

macro_rules! require {
    ($cond:expr, $($arg:tt)*) => {
        if $cond {} else { return Err(format!($($arg)*)); }
    };
}

/// Assert a channel's captured audio is dominated by `expected_hz`, and that none
/// of the wrong-speed candidate frequencies carries comparable energy. The
/// candidates cover a doubling/halving (gross ratio error) and the 44.1↔48 kHz
/// mismatch (an 8.8 % pitch shift — the classic "sounds slightly off" bug).
fn assert_tone(samples: &[i32], rate: f64, expected_hz: f64, tag: &str) -> Result<(), String> {
    let p_exp = goertzel_power(samples, rate, expected_hz);
    let wrong = [
        expected_hz * 0.5,
        expected_hz * 2.0,
        expected_hz * 48_000.0 / 44_100.0,
        expected_hz * 44_100.0 / 48_000.0,
    ];
    let p_wrong = wrong
        .iter()
        .map(|&hz| goertzel_power(samples, rate, hz))
        .fold(0.0f64, f64::max);
    require!(
        p_exp > p_wrong * 10.0,
        "{tag}: tone not at {expected_hz:.2} Hz — power there {p_exp:.3e} not \
         >10x the best wrong-speed candidate {p_wrong:.3e} (candidates {wrong:?}); \
         playback speed/decimation is off"
    );
    Ok(())
}

/// Drive tone.dsf end-to-end into a `sink_rate`-matched sink and assert the
/// captured stereo audio is the correct 440/659.25 Hz tone at the right speed.
fn audit_dsd_at(sink_rate: u32) -> Result<(), String> {
    let path = testdata_dir().join("tone.dsf");
    require!(path.is_file(), "committed fixture {path:?} is missing");

    // The decoder's native output must be DSD's 44.1 kHz (every family).
    let si = open_track(&path)
        .map_err(|e| format!("open_track probe: {e}"))?
        .stream_info();
    require!(
        si.sample_rate == NATIVE_RATE && si.channels == 2,
        "tone.dsf stream_info {} Hz/{}ch ≠ expected {NATIVE_RATE} Hz/2ch (decimation wrong)",
        si.sample_rate,
        si.channels
    );
    let ch = 2usize;

    let deck = Deck::new(
        CaptureSink {
            rate: sink_rate,
            channels: 2,
            written: Vec::new(),
        },
        Quality::Hq,
    );
    let engine = Engine::new(deck, queue_for(vec![0]));
    let mut player: HarnessPlayer = Player::new(engine, u64::from(sink_rate));
    apply_command(
        &mut player,
        Command::PlayContext {
            ids: vec![0],
            start: 0,
        },
        0,
    )
    .map_err(|e| format!("PlayContext: {e:?}"))?;

    // Render under the 0.5 s length so the track is still live when we query it.
    let want_frames = (RENDER_SECS * f64::from(sink_rate)) as usize;
    pump_capture_frames(&mut player, &path, want_frames, ch);

    // Duration end-to-end, as the UI's seek bar sees it: a live DSD track must
    // report a real length. (The fresh decoder's declared length is printed too,
    // to localise any future regression to the decoder vs. the deck/player path.)
    let fresh_tf = SeekableDecoder::total_frames(
        &open_track(&path).map_err(|e| format!("open_track dur-probe: {e}"))?,
    );
    let d = project_tick(&player, 1.0, false)
        .duration_secs
        .ok_or_else(|| {
            format!("no duration while DSD track is live (decoder.total_frames={fresh_tf:?})")
        })?;
    require!(
        (d - KNOWN_SECS).abs() < DUR_TOL_SECS,
        "duration {d:.3}s not within {DUR_TOL_SECS}s of known {KNOWN_SECS:.3}s \
         (decoder.total_frames={fresh_tf:?})"
    );
    println!("  sink@{sink_rate}Hz: duration={d:.3}s (decoder.total_frames={fresh_tf:?})");

    let written = &player.deck().sink().written;
    let (peak, rms) = peak_and_rms(written);
    require!(
        (PLAYS_PEAK_FLOOR..=PLAYS_PEAK_CEIL).contains(&peak) && rms > PLAYS_RMS_FLOOR,
        "DSD audio peak={peak} rms={rms:.0} outside sane 24-bit range — silent/clipped?"
    );

    // Analyse a steady window well past the deck's 8 ms fade-in.
    let skip = (sink_rate as usize / 20) * ch; // ~50 ms
    let analysis = &written[skip.min(written.len())..];
    require!(
        analysis.len() >= sink_rate as usize * ch / 4,
        "too little steady audio captured for analysis ({} samples)",
        analysis.len()
    );
    let l = channel(analysis, ch, 0);
    let r = channel(analysis, ch, 1);
    let rate_f = f64::from(sink_rate);

    assert_tone(&l, rate_f, 440.0, &format!("sink@{sink_rate}Hz LEFT"))?;
    assert_tone(&r, rate_f, 659.25, &format!("sink@{sink_rate}Hz RIGHT"))?;

    // Channel identity: LEFT is 440 (not 659), RIGHT is 659 (not 440) — no swap.
    require!(
        goertzel_power(&l, rate_f, 440.0) > goertzel_power(&l, rate_f, 659.25) * 10.0
            && goertzel_power(&r, rate_f, 659.25) > goertzel_power(&r, rate_f, 440.0) * 10.0,
        "sink@{sink_rate}Hz: channels swapped (L/R tones crossed)"
    );

    println!(
        "  sink@{sink_rate}Hz: peak={peak} rms={rms:.0} — L=440Hz R=659.25Hz at correct speed OK"
    );
    Ok(())
}

#[test]
fn dsd_plays_at_correct_speed_end_to_end() {
    println!("\n=== END-TO-END DSD PLAYBACK (ServicePlayer → Deck → CaptureSink) ===");
    let mut failures = Vec::new();
    // 44.1 kHz: deck pass-through (DSD native). 48 kHz: deck resamples DSD output.
    for rate in [44_100u32, 48_000] {
        if let Err(e) = on_big_stack(move || audit_dsd_at(rate)) {
            failures.push(format!("sink@{rate}Hz: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "DSD end-to-end playback failures:\n  {}",
        failures.join("\n  ")
    );
    println!("=== DSD playback audit complete ===\n");
}
