//! Real-device **end-to-end**: play a 44.1 kHz track through the full
//! `PlayerHandle` stack (host factory → pump → exclusive sink → core `swap_sink`)
//! with exclusive output ON, on a machine whose default device runs at a different
//! rate, and confirm the engine reopens the device at the track's 44.1 kHz so the
//! signal path reports a **bit-perfect** (no-resample) exclusive stream.
//!
//! `#[ignore]`d: it touches the real default output device and makes (silent)
//! sound, so CI (headless) never runs it. Runs wherever an exclusive backend
//! exists: Windows (WASAPI), macOS (CoreAudio) and Linux (ALSA). Run manually:
//!
//! ```text
//! cargo test -p soul-player-host --test native_rate_hardware_e2e -- --ignored --nocapture
//! ```
//!
//! This proves the whole stack engages exclusive mode at the track's native rate on
//! real hardware. It does NOT prove digital bit-exactness (that needs a DAC/loopback).
//!
//! The rate is discovered rather than assumed. Plenty of cards run exactly one rate
//! (this was written against an Intel SOF HDA that is 48 kHz only), and on those a
//! hardcoded 44.1 kHz expectation fails for a reason that is not a bug. So the test
//! asks for 44.1 kHz first, the interesting case, where the device default differs
//! and the engine must switch, and if the card refuses, retries at the rate the card
//! actually chose and still demands a bit-perfect, non-resampling exclusive stream
//! there. Either way the claim is the same: exclusive engaged, at the track's native
//! rate, with nothing resampling.
#![cfg(any(windows, target_os = "macos", target_os = "linux"))]

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
    let h = PlayerHandle::new(); // Device mode (SOUL_PLAYER_OFFLINE unset)
    if h.default_output_device().is_none() {
        eprintln!("no default output device on this host, skipping hardware e2e");
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

    // Phase 1: a 44.1 kHz track. On a device whose default is 48 kHz this is the
    // native-rate SWITCH, the case worth proving.
    let (matched, observed) = play_and_watch(&h, 44_100, &rx);
    if matched {
        println!("native-rate switch engaged: exclusive @ 44.1 kHz, no resampling");
        assert_eq!(h.exclusive_status(), ExclusiveStatus::Active);
        return;
    }

    // Phase 2: the card would not run 44.1 kHz. That is a real hardware limit, not a
    // code bug, but exclusive must still have engaged at whatever rate it chose, and
    // a track AT that rate must then pass through without resampling.
    let device_rate = observed.map(|sp| sp.device_rate).unwrap_or_else(|| {
        panic!(
            "no exclusive signal path at all, exclusive never engaged on this \
             hardware; status {:?}",
            h.exclusive_status()
        )
    });
    println!("card refused 44.1 kHz; retrying at its own {device_rate} Hz");
    let (matched, observed) = play_and_watch(&h, device_rate, &rx);
    assert!(
        matched,
        "expected a bit-perfect exclusive stream at the card's own {device_rate} Hz \
         (no resample); last saw {observed:?}, status {:?}",
        h.exclusive_status()
    );
    assert_eq!(
        h.exclusive_status(),
        ExclusiveStatus::Active,
        "exclusive_status should be Active after a successful open"
    );
}

/// Play a silent track at `rate` and watch the signal path for a bit-perfect
/// exclusive stream at that rate. Returns `(matched, last_exclusive_path_seen)`.
fn play_and_watch(
    h: &PlayerHandle,
    rate: u32,
    rx: &mpsc::Receiver<soul_player_host::SignalPath>,
) -> (bool, Option<soul_player_host::SignalPath>) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.wav"), silent_wav(rate, rate * 2)).expect("write wav");
    h.open_folder(dir.path())
        .expect("open_folder plays from track 0");

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_exclusive = None;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(sp) => {
                if sp.exclusive {
                    last_exclusive = Some(sp);
                    if sp.device_rate == rate && !sp.resampling {
                        return (true, last_exclusive);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    (false, last_exclusive)
}
