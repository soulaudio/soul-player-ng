//! The flags that must answer without a display.
//!
//! `--version` used to fall through to the event loop and panic with "neither
//! WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set", which made it
//! useless exactly where a version flag earns its keep: a packaging check, a
//! container, a CI box. The installer tests now read the version out of the
//! package manager instead, but the flag should work regardless.
#![forbid(unsafe_code)]

use std::process::Command;

/// Run the real binary with no display of any kind in the environment.
fn headless(args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_soul-player"));
    cmd.args(args);
    // Strip every way a display could be found, so this test means the same
    // thing on a developer's desktop as it does on a headless runner.
    for var in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "WAYLAND_SOCKET",
        "XDG_SESSION_TYPE",
    ] {
        cmd.env_remove(var);
    }
    cmd.output().expect("the binary runs")
}

#[test]
fn version_prints_and_exits_without_a_display() {
    let out = headless(&["--version"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        out.status.success(),
        "--version exited {:?}\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code(),
    );
    assert!(
        !stderr.contains("panicked"),
        "--version panicked:\n{stderr}"
    );
    // The exact string packaging and support requests quote back.
    let expected = format!("soul-player {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(stdout, expected, "stderr was: {stderr}");
}

#[test]
fn short_v_is_the_same_flag() {
    let long = headless(&["--version"]);
    let short = headless(&["-V"]);
    assert_eq!(short.status.code(), long.status.code());
    assert_eq!(short.stdout, long.stdout);
}
