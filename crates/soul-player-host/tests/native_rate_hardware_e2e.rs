//! Real-device **end-to-end**: play a 44.1 kHz track through the full
//! `PlayerHandle` stack (host factory → pump → exclusive sink → core `swap_sink`)
//! with exclusive output ON, on a machine whose default device runs at a different
//! rate, and confirm the engine reopens the device at the track's 44.1 kHz so the
//! signal path reports a **bit-perfect** (no-resample) exclusive stream.
//!
//! `#[ignore]`d — it touches the real default output device and makes (silent)
//! sound, so CI (headless) never runs it. Windows only (exclusive output is WASAPI).
//! Run manually:
//!
//! ```text
//! cargo test -p soul-player-host --test native_rate_hardware_e2e -- --ignored --nocapture
//! ```
//!
//! This proves the whole stack engages exclusive mode at the track's native rate on
//! real hardware. It does NOT prove digital bit-exactness (that needs a DAC/loopback).
#![cfg(windows)]

use std::sync::mpsc;
use std::time::{Duration, Instant};

use soul_player_host::{Event, ExclusiveStatus, PlayerHandle};

/// A silent stereo 16-bit WAV of `frames` samples at `rate` (no audible output).
fn silent_wav(rate: u32, frames: u32) -> Vec<u8> {
    let channels: u16 = 2;
    let block_align = channels * 2;
    let byte_rate = rate * u32::from(block_align);
    let data_len = frames * u32::from(block_align);
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&byte_rate.to_le_bytes());
    v.extend_from_slice(&block_align.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.resize(v.len() + data_len as usize, 0); // silence
    v
}

#[test]
#[ignore = "touches the real default output device; run manually with --ignored"]
fn native_rate_switch_engages_on_real_hardware() {
    // A 2-second silent 44.1 kHz track.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.wav"), silent_wav(44_100, 88_200)).expect("write wav");

    let h = PlayerHandle::new(); // Device mode (SOUL_PLAYER_OFFLINE unset)
    if h.default_output_device().is_none() {
        eprintln!("no default output device on this host — skipping hardware e2e");
        return;
    }
    h.set_exclusive(true);

    // Collect the live signal-path projections the pump emits each tick.
    let (tx, rx) = mpsc::channel();
    h.subscribe(move |ev| {
        if let Event::SignalPath(Some(sp)) = ev {
            let _ = tx.send(sp);
        }
    });

    h.open_folder(dir.path())
        .expect("open_folder plays from track 0");

    // Wait (bounded) for a signal path showing exclusive active at the track's native
    // 44.1 kHz with no resampling — i.e. the native-rate switch engaged on hardware.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = None;
    let mut matched = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(sp) => {
                last = Some(sp);
                if sp.exclusive && sp.device_rate == 44_100 && !sp.resampling {
                    matched = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    println!("final signal path: {last:?}");
    println!("exclusive status:  {:?}", h.exclusive_status());
    assert!(
        matched,
        "expected exclusive @ 44.1 kHz (bit-perfect, no resample) after the \
         native-rate switch on real hardware; last saw {last:?}, status {:?}. \
         (If this device can't open exclusive at 44.1 kHz, that's a real fallback, \
         not a code bug — check the wasapi_device_probe output.)",
        h.exclusive_status()
    );
    // Sanity: the status cell agrees the stream is exclusive (active at some rate).
    assert!(
        matches!(h.exclusive_status(), ExclusiveStatus::Active),
        "exclusive_status should be Active after a successful switch"
    );
}
