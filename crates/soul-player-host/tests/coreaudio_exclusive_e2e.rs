//! macOS real-device **end-to-end** for CoreAudio exclusive output, through the whole
//! `PlayerHandle` stack (host factory → pump → `CoreAudioExclusiveSink` → core deck):
//!
//! - a folder of tracks at 44.1 / 48 / 88.2 / 96 / 192 kHz plays with the DAC switched to
//!   each track's native rate (bit-perfect: exclusive, no resampling), and a rate the
//!   device can't run (192 k on a 96 k-max DAC) is mapped to the closest same-family rate
//!   and HONESTLY reported as resampling;
//! - the device is hogged by this process while exclusive, and released (and its original
//!   nominal rate restored) when exclusive is switched off mid-playback, with playback
//!   continuing in shared mode;
//! - strict mode with the device hogged by another process refuses to start rather than
//!   silently downgrading.
//!
//! `#[ignore]`d: it takes over a real output device (writing SILENCE only). Run:
//!
//! ```text
//! cargo test --test coreaudio_exclusive_e2e -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `SOUL_CA_DEVICE=<name>` picks the device (e.g. `"BlackHole 2ch"` in CI); default is the
//! system default output, resolved to a name once up front (macOS moves the system default
//! off a hogged device, so it must not be re-resolved mid-test).
#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use soul_audio_desktop::exclusive_policy::{choose_rate, supported_rates};
use soul_audio_desktop::{hog_owner, nominal_rate, physical_format_ladder};
use soul_player_host::{Event, ExclusiveStatus, PlayerHandle, SignalPath};

static SERIAL: Mutex<()> = Mutex::new(());

fn device() -> Option<String> {
    static DEV: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    DEV.get_or_init(|| {
        std::env::var("SOUL_CA_DEVICE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(soul_audio_desktop::default_output_device_name)
    })
    .clone()
}

/// A silent stereo 16-bit WAV of `secs` at `rate`.
fn silent_wav(rate: u32, secs: f32) -> Vec<u8> {
    let frames = (rate as f32 * secs) as u32;
    let data_len = frames * 4;
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&2u16.to_le_bytes()); // stereo
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 4).to_le_bytes());
    v.extend_from_slice(&4u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.resize(v.len() + data_len as usize, 0);
    v
}

/// A player on the device under test with exclusive on, and a channel of every signal
/// path the pump emits. `None` (skip) on a machine with no output device.
fn player(strict: bool) -> Option<(PlayerHandle, mpsc::Receiver<SignalPath>)> {
    let name = device()?;
    let h = PlayerHandle::new();
    h.set_output_device(Some(name));
    h.set_exclusive_strict(strict);
    h.set_exclusive(true);
    let (tx, rx) = mpsc::channel();
    h.subscribe(move |ev| {
        if let Event::SignalPath(Some(sp)) = ev {
            let _ = tx.send(sp);
        }
    });
    Some((h, rx))
}

fn wait_for(
    rx: &mpsc::Receiver<SignalPath>,
    secs: u64,
    mut pred: impl FnMut(&SignalPath) -> bool,
) -> Option<SignalPath> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Ok(sp) = rx.recv_timeout(Duration::from_millis(100))
            && pred(&sp)
        {
            return Some(sp);
        }
    }
    None
}

#[test]
#[ignore = "takes over a real output device; run with --ignored"]
fn every_track_plays_at_its_native_rate_or_an_honest_fallback() {
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let ladder = supported_rates(&physical_format_ladder(device().as_deref()), 2);
    let track_rates = [44_100u32, 48_000, 88_200, 96_000, 192_000];
    let dir = tempfile::tempdir().unwrap();
    for (i, r) in track_rates.iter().enumerate() {
        std::fs::write(
            dir.path().join(format!("{i:02}-{r}.wav")),
            silent_wav(*r, 1.5),
        )
        .unwrap();
    }
    let Some((h, rx)) = player(false) else {
        eprintln!("SKIP: no output device");
        return;
    };
    let original = nominal_rate(device().as_deref());
    h.open_folder(dir.path()).expect("plays from track 0");

    // decoded rate → (device rate, resampling, hw nominal rate at that moment)
    let seen = Arc::new(Mutex::new(BTreeMap::<u32, (u32, bool, Option<u32>)>::new()));
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline && seen.lock().unwrap().len() < track_rates.len() {
        let Ok(sp) = rx.recv_timeout(Duration::from_millis(100)) else {
            continue;
        };
        if !sp.exclusive {
            continue;
        }
        let want = choose_rate(&ladder, sp.decoded_rate).unwrap();
        if sp.device_rate == want {
            let hw = nominal_rate(device().as_deref());
            seen.lock().unwrap().entry(sp.decoded_rate).or_insert((
                sp.device_rate,
                sp.resampling,
                hw,
            ));
        }
    }
    let seen = seen.lock().unwrap().clone();
    println!("ladder {ladder:?}\nseen   {seen:#?}");
    assert_eq!(
        hog_owner(device().as_deref()),
        Some(std::process::id() as i32),
        "player hogs the device while exclusive"
    );
    for r in track_rates {
        let want = choose_rate(&ladder, r).unwrap();
        let (dev, resampling, hw) = *seen
            .get(&r)
            .unwrap_or_else(|| panic!("never saw the {r} Hz track play exclusive at {want}"));
        assert_eq!(dev, want, "{r} Hz track → device at {want}");
        assert_eq!(
            resampling,
            want != r,
            "{r} Hz: resampling flag must be honest (native ⇒ bit-perfect)"
        );
        assert_eq!(hw, Some(want), "{r} Hz: the hardware itself is at {want}");
    }
    assert!(
        matches!(h.exclusive_status(), ExclusiveStatus::Active),
        "status Active, got {:?}",
        h.exclusive_status()
    );

    // Switch exclusive OFF mid-playback: hog released, original rate back, playback
    // continues in shared mode.
    h.set_exclusive(false);
    let shared = wait_for(&rx, 10, |sp| !sp.exclusive).expect("playback continues in shared mode");
    println!("after exclusive off: {shared:?}");
    assert_eq!(hog_owner(device().as_deref()), Some(-1), "hog released");
    assert_eq!(
        nominal_rate(device().as_deref()),
        original,
        "original nominal rate restored"
    );
    assert!(matches!(h.exclusive_status(), ExclusiveStatus::Shared));
    drop(h);
}

/// Helper process: hogs the device under test until killed.
#[test]
#[ignore = "helper for strict_mode_refuses_to_play_when_another_app_hogs_the_device"]
fn zz_helper_hold_device() {
    if std::env::var_os("SOUL_CA_HOLD").is_none() {
        return;
    }
    let _s = soul_audio_desktop::CoreAudioExclusiveSink::open(device(), 48_000, 2).unwrap();
    println!("HOLDING");
    std::thread::sleep(Duration::from_secs(30));
}

#[test]
#[ignore = "takes over a real output device; run with --ignored"]
fn strict_mode_refuses_to_play_when_another_app_hogs_the_device() {
    use std::io::{BufRead, BufReader};
    let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if device().is_none() {
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "zz_helper_hold_device",
            "--nocapture",
        ])
        .env("SOUL_CA_HOLD", "1")
        .env("SOUL_CA_DEVICE", device().unwrap())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Keep the reader alive until the child is killed, so its later output never hits a
    // closed pipe.
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    assert!(
        lines.any(|l| l.unwrap().contains("HOLDING")),
        "helper hogged the device"
    );

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.wav"), silent_wav(44_100, 2.0)).unwrap();

    // Strict: never downgrade: status Unavailable, no exclusive signal path.
    let (h, rx) = player(true).unwrap();
    let _ = h.open_folder(dir.path());
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline
        && !matches!(h.exclusive_status(), ExclusiveStatus::Unavailable(_))
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let st = h.exclusive_status();
    println!("strict status: {st:?}");
    assert!(
        matches!(&st, ExclusiveStatus::Unavailable(r) if r.contains("in use")),
        "strict + foreign hog ⇒ Unavailable(in use), got {st:?}"
    );
    assert!(
        wait_for(&rx, 1, |sp| sp.exclusive).is_none(),
        "no exclusive playback happened"
    );
    drop(h);

    // Non-strict: falls back to shared, with the reason surfaced.
    let (h, rx) = player(false).unwrap();
    h.open_folder(dir.path()).unwrap();
    let sp = wait_for(&rx, 5, |sp| !sp.exclusive);
    let st = h.exclusive_status();
    println!("non-strict status: {st:?}");
    assert!(
        matches!(&st, ExclusiveStatus::FellBack(r) if r.contains("in use")),
        "non-strict + foreign hog ⇒ FellBack(in use), got {st:?}"
    );
    assert!(sp.is_some(), "shared playback still runs");
    drop(h);

    child.kill().unwrap();
    child.wait().unwrap();
    drop(lines);
}
