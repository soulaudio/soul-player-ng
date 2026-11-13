//! END-TO-END **signal-path matrix**: every committed `tone.*` fixture driven
//! through the real `ServicePlayer` → `Deck` (no audio device), asserting
//! `project_signal_path` reports the correct chain for each **format × device
//! rate**. This is the in-depth counterpart to the DSD-only `dsd_playback_e2e`:
//! it proves the signal path is right across PCM (WAV/FLAC/ALAC/AIFF/MP3/AAC/
//! MP2/Opus/MP1) and every DSD family (DSD64/128/256/512), at both a 44.1 kHz
//! device (so same-rate fixtures are sample-rate-exact) and a 48 kHz device (so
//! 44.1/32 kHz fixtures — and all DSD, which decodes to 44.1 kHz — resample).
//!
//! Invariants per case: source KIND (PCM vs the exact native DSD rate), the
//! decoded PCM rate, channel count, and `resampling == (decoded_rate != device)`.
//! Runs on a 64 MiB stack (the owned decoder is large by value).

use soul_audio_control::Player;
use soul_audio_core::{Deck, Quality, SampleFormat, SinkError, StreamInfo};
use soul_audio_desktop::{OwnedDecoder, open_track};
use soul_audio_engine::Engine;
use std::path::PathBuf;

use soul_player_backend::driver::queue_for;
use soul_player_backend::{
    Command, ServicePlayer, SourceFormat, apply_command, project_signal_path,
};

/// A device-less sink at a fixed rate/channels; discards audio (we only inspect
/// the signal-path projection, not the samples).
struct NullSink {
    rate: u32,
    channels: u16,
}
impl soul_audio_core::Sink for NullSink {
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
    fn discard(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
    fn set_volume(&mut self, _v: f32) {}
}

type MatrixPlayer = ServicePlayer<NullSink, OwnedDecoder>;

fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// One fixture + its expected chain facts. `dsd_rate_hz` is `Some` for DSD (the
/// native 1-bit rate the source should report); `None` = PCM. `decoded_rate` is
/// the PCM rate the decoder hands the deck (DSD always lands on 44.1 kHz).
struct Case {
    file: &'static str,
    dsd_rate_hz: Option<u32>,
    decoded_rate: u32,
}

#[rustfmt::skip]
const CASES: &[Case] = &[
    Case { file: "tone.wav",        dsd_rate_hz: None,               decoded_rate: 44_100 },
    Case { file: "tone.flac",       dsd_rate_hz: None,               decoded_rate: 44_100 },
    Case { file: "tone.m4a",        dsd_rate_hz: None,               decoded_rate: 44_100 }, // ALAC
    Case { file: "tone.aif",        dsd_rate_hz: None,               decoded_rate: 44_100 }, // AIFF PCM
    Case { file: "tone.mp3",        dsd_rate_hz: None,               decoded_rate: 44_100 },
    Case { file: "tone.aac",        dsd_rate_hz: None,               decoded_rate: 44_100 }, // ADTS
    Case { file: "tone.mp2",        dsd_rate_hz: None,               decoded_rate: 44_100 },
    Case { file: "tone.opus",       dsd_rate_hz: None,               decoded_rate: 48_000 },
    Case { file: "tone.mp1",        dsd_rate_hz: None,               decoded_rate: 32_000 },
    Case { file: "tone.dsf",        dsd_rate_hz: Some(2_822_400),    decoded_rate: 44_100 }, // DSD64
    Case { file: "tone.dff",        dsd_rate_hz: Some(2_822_400),    decoded_rate: 44_100 }, // DSD64 (DFF)
    Case { file: "tone-dsd128.dsf", dsd_rate_hz: Some(5_644_800),    decoded_rate: 44_100 },
    Case { file: "tone-dsd256.dsf", dsd_rate_hz: Some(11_289_600),   decoded_rate: 44_100 },
    Case { file: "tone-dsd512.dsf", dsd_rate_hz: Some(22_579_200),   decoded_rate: 44_100 },
];

const DEVICE_RATES: [u32; 2] = [44_100, 48_000];

/// Drive the player until a track is loaded (bounded), servicing opens with the
/// one real file.
fn load(p: &mut MatrixPlayer, path: &std::path::Path) {
    for _ in 0..100_000 {
        while let Some(id) = p.poll_open() {
            let dec = open_track(path).expect("open_track");
            p.provide(id, dec).expect("provide");
        }
        p.pump().expect("pump");
        if project_signal_path(p).is_some() {
            break;
        }
    }
}

fn check(case: &Case, device_rate: u32) -> Result<(), String> {
    let path = testdata_dir().join(case.file);
    if !path.is_file() {
        return Err(format!("{}: fixture missing", case.file));
    }
    let deck = Deck::new(
        NullSink {
            rate: device_rate,
            channels: 2,
        },
        Quality::Hq,
    );
    let engine = Engine::new(deck, queue_for(vec![0]));
    let mut player: MatrixPlayer = Player::new(engine, u64::from(device_rate));
    apply_command(
        &mut player,
        Command::PlayContext {
            ids: vec![0],
            start: 0,
        },
        0,
    )
    .map_err(|e| format!("{}: PlayContext {e:?}", case.file))?;
    load(&mut player, &path);

    let sp = project_signal_path(&player)
        .ok_or_else(|| format!("{}@{device_rate}: no signal path after load", case.file))?;

    // Source kind: exact native DSD rate, or PCM of some bit depth.
    match (case.dsd_rate_hz, sp.source) {
        (Some(hz), SourceFormat::Dsd { rate_hz }) if rate_hz == hz => {}
        (None, SourceFormat::Pcm { .. }) => {}
        (_, got) => {
            return Err(format!(
                "{}@{device_rate}: source {got:?}, expected {:?}",
                case.file,
                case.dsd_rate_hz.map(|hz| SourceFormat::Dsd { rate_hz: hz })
            ));
        }
    }
    if sp.decoded_rate != case.decoded_rate {
        return Err(format!(
            "{}@{device_rate}: decoded_rate {} != {}",
            case.file, sp.decoded_rate, case.decoded_rate
        ));
    }
    if sp.channels != 2 {
        return Err(format!(
            "{}@{device_rate}: channels {}",
            case.file, sp.channels
        ));
    }
    if sp.device_rate != device_rate {
        return Err(format!(
            "{}@{device_rate}: device_rate {}",
            case.file, sp.device_rate
        ));
    }
    let want_resample = case.decoded_rate != device_rate;
    if sp.resampling != want_resample {
        return Err(format!(
            "{}@{device_rate}: resampling {} (decoded {} vs device {device_rate}), want {want_resample}",
            case.file, sp.resampling, case.decoded_rate
        ));
    }
    println!(
        "  OK {}@{device_rate}: {:?} → {} kHz · resample={}",
        case.file,
        sp.source,
        sp.decoded_rate / 1000,
        sp.resampling
    );
    Ok(())
}

#[test]
fn signal_path_is_correct_across_formats_and_device_rates() {
    let run = || -> Result<(), String> {
        println!(
            "\n=== SIGNAL-PATH MATRIX ({} formats × {} device rates) ===",
            CASES.len(),
            DEVICE_RATES.len()
        );
        let mut failures = Vec::new();
        for case in CASES {
            for device_rate in DEVICE_RATES {
                if let Err(e) = check(case, device_rate) {
                    failures.push(e);
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{} signal-path mismatch(es):\n  {}",
                failures.len(),
                failures.join("\n  ")
            ))
        }
    };
    // 64 MiB stack: OwnedDecoder is large by value through the open→provide→deck chain.
    let res = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn_scoped(s, run)
            .expect("spawn")
            .join()
            .expect("matrix thread panicked")
    });
    assert!(res.is_ok(), "{}", res.unwrap_err());
}
