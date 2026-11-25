//! Linux desktop-session acceptance E2E: launch the REAL binary and check what
//! it actually presents to the desktop.
//!
//! Everything here is observed from OUTSIDE the process, the way a desktop
//! environment sees it, over the D-Bus session bus, and through the process's
//! own exit behaviour. That is the point: the app's unit and view tests already
//! cover its internals thoroughly, but none of them can catch the failures that
//! only exist at the session boundary. Every bug this file guards against
//! shipped past a green unit suite:
//!
//! * MPRIS never appearing, or appearing without the track metadata;
//! * artwork published as a URL pointing at a file that isn't there;
//! * `Stop` reporting `Paused`, which is a different MPRIS status;
//! * SIGTERM killing the process before it could mark a clean exit, so every
//!   logout made the next launch claim the previous session had crashed.
//!
//! These need a real session bus (and the app needs an audio sink and a place
//! to put a window), so they are `#[ignore]` by default and run explicitly:
//!
//! ```sh
//! dbus-run-session -- xvfb-run -a cargo test --test linux_session_e2e -- --ignored
//! ```
//!
//! CI runs them inside its headless Wayland job. They are NOT in the default
//! `cargo test` set because a developer machine may have neither, and a test
//! that silently skips is worse than one that is explicitly opt-in.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedValue};

/// The MPRIS bus name the app registers (`media_session::DBUS_NAME`).
///
/// Built from the binary name for the same reason the app builds it that way:
/// the suffix must stay equal to the desktop entry's basename, or a media applet
/// has no way to find the player's icon (MPRIS's `DesktopEntry` property, which
/// would say so outright, is not implemented by souvlaki). Spelling it out here
/// would let the two drift silently, and this test would still pass, because it
/// would simply be asserting against whatever the app happened to register.
fn bus() -> String {
    format!("org.mpris.MediaPlayer2.{}", bin_name())
}

/// The built binary's file stem, what `main` passes to `rune::set_app_id`, what
/// cargo-packager names the desktop entry after, and (now) the MPRIS bus-name
/// suffix. `CARGO_BIN_NAME` is not set for an integration-test target, so this
/// reads it off the binary path Cargo does provide.
fn bin_name() -> &'static str {
    Path::new(env!("CARGO_BIN_EXE_soul-player"))
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("the built binary has a name")
}
const OBJECT: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";

/// How long to let the app reach a state before calling it a failure. Generous:
/// a cold debug build starting a GPU context on a loaded CI runner is slow, and
/// a flaky timeout would be worse than a slow test.
const SETTLE: Duration = Duration::from_secs(45);

/// Poll `f` until it yields `Some`, or fail with `what` after [`SETTLE`].
fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + SETTLE;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {SETTLE:?} waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The repo's committed fixtures.
fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdata")
}

/// A running app, with its own throwaway XDG directories.
///
/// Isolated on purpose: the app persists settings, a scan cache and the
/// running-session sentinel under `$XDG_*`, and a test must neither read the
/// developer's real state nor leave anything in it. (An early version of this
/// harness didn't, and a stale sentinel from a killed run put a crash-report
/// modal over the window on the next launch.)
struct App {
    child: Child,
    home: tempfile::TempDir,
}

impl App {
    /// Launch over `library`, optionally starting playback immediately.
    fn launch(library: &Path, autoplay: bool) -> Self {
        let home = tempfile::tempdir().expect("create a throwaway XDG home");
        for sub in ["config", "data", "state", "cache"] {
            std::fs::create_dir_all(home.path().join(sub)).expect("create an XDG subdir");
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_soul-player"));
        cmd.env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_STATE_HOME", home.path().join("state"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("SOUL_PLAYER_FOLDER", library)
            // Never let a test hit the update endpoint.
            .env("SOUL_PLAYER_NO_UPDATE", "1")
            .env("RUNE_PERF_WARN", "0");
        if autoplay {
            cmd.env("SOUL_PLAYER_AUTOPLAY", "1");
        }
        let child = cmd.spawn().expect("spawn the player binary");
        Self { child, home }
    }

    /// The running-session sentinel. Present while the app runs; removed on a
    /// CLEAN exit, its survival is exactly what makes the next launch report
    /// that the previous session crashed.
    fn sentinel(&self) -> PathBuf {
        self.home.path().join("state/soul-player/logs/running.lock")
    }

    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Send `signal` and wait for the process to go away.
    fn signal_and_wait(&mut self, signal: &str) -> bool {
        let _ = Command::new("kill")
            .args([format!("-{signal}"), self.child.id().to_string()])
            .status();
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Never leave a GUI process behind, however the test ended.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A blocking session-bus connection, or a clear failure. The test is
/// `#[ignore]`d precisely so that "no session bus" means "you didn't set this
/// up", not a silent pass.
fn session_bus() -> Connection {
    Connection::session().expect(
        "no D-Bus session bus, run under `dbus-run-session -- …` \
         (see this file's module docs)",
    )
}

/// Wait until the app owns the MPRIS name, so property reads can't race startup.
fn wait_for_mpris(conn: &Connection) {
    let proxy = zbus::blocking::fdo::DBusProxy::new(conn).expect("open the D-Bus proxy");
    wait_for("the app to claim its MPRIS bus name", || {
        let names = proxy.list_names().ok()?;
        names.iter().any(|n| n.as_str() == bus()).then_some(())
    });
}

fn get(conn: &Connection, iface: &str, prop: &str) -> OwnedValue {
    zbus::blocking::fdo::PropertiesProxy::builder(conn)
        .destination(bus().as_str())
        .expect("destination")
        .path(OBJECT)
        .expect("path")
        .build()
        .expect("build the properties proxy")
        .get(iface.try_into().expect("interface name"), prop)
        .unwrap_or_else(|e| panic!("read {iface}.{prop}: {e}"))
}

fn playback_status(conn: &Connection) -> String {
    String::try_from(get(conn, PLAYER_IFACE, "PlaybackStatus")).expect("PlaybackStatus is a string")
}

/// One entry of the `Metadata` dict as a string, or `None` when absent.
fn metadata_str(conn: &Connection, key: &str) -> Option<String> {
    let md = get(conn, PLAYER_IFACE, "Metadata");
    let dict: std::collections::HashMap<String, OwnedValue> = md.try_into().ok()?;
    let value = dict.get(key)?;
    // `xesam:artist` is a string ARRAY; the rest are plain strings.
    if let Ok(s) = String::try_from(value.try_clone().ok()?) {
        return Some(s);
    }
    let list: Vec<String> = value.try_clone().ok()?.try_into().ok()?;
    list.first().cloned()
}

/// `Position` in microseconds, as MPRIS reports it.
fn position_us(conn: &Connection) -> i64 {
    i64::try_from(get(conn, PLAYER_IFACE, "Position")).expect("Position is an int64")
}

fn call(conn: &Connection, method: &str) {
    conn.call_method(
        Some(bus().as_str()),
        OBJECT,
        Some(PLAYER_IFACE),
        method,
        &(),
    )
    .unwrap_or_else(|e| panic!("call {method}: {e}"));
}

/// Roughly how long autoplay must keep producing audio for the session
/// assertions to be observable.
///
/// The app starts playing on its FIRST FRAME, but the OS media session only
/// comes up at `on_ready`, after the GPU context, which can take several
/// seconds in a headless CI run. The committed fixtures are ~1.5 s each, so a
/// single one is over before anything can watch it, and the test sees a player
/// that never played. Staging a run of copies makes `PlayContext` walk them
/// back to back, which is the same path a real "Play All" takes.
const PLAYING_SECONDS: f64 = 90.0;
/// A one-album library holding enough copies of `fixture` to keep playing for
/// [`PLAYING_SECONDS`]. Copies rather than one long file so the suite needs no
/// fixture that isn't already committed.
///
/// `fixture_secs` is the fixture's own length and MUST be right: the committed
/// fixtures are not all the same length (`meta.*` run ~1.5 s, but `arted.flac`
/// is 0.2 s), and assuming one duration for all of them staged a library that
/// finished playing before the OS media session had even started, a failure
/// that looked like "playback never began".
fn staged_library(fixture: &str, extension: &str, fixture_secs: f64) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create a library temp dir");
    let album = dir.path().join("Open Goldberg Variations");
    std::fs::create_dir_all(&album).expect("create the album dir");
    let source = testdata_dir().join(fixture);
    let copies = (PLAYING_SECONDS / fixture_secs).ceil() as usize;
    for i in 0..copies {
        std::fs::copy(&source, album.join(format!("aria-{i:04}.{extension}")))
            .unwrap_or_else(|e| panic!("stage {fixture}: {e}"));
    }
    dir
}

/// The app must publish itself on MPRIS with the CURRENT TRACK's tags, the
/// thing a lock screen, a media applet and `playerctl` all read.
///
/// Asserts the tags rather than just "a name appeared": the session existing
/// while advertising an empty title is a failure a presence check waves through.
#[test]
#[ignore = "needs a D-Bus session bus and a display; see the module docs"]
fn publishes_the_now_playing_track_on_mpris() {
    let library = staged_library("meta.flac", "flac", 1.5);
    let mut app = App::launch(library.path(), true);
    let conn = session_bus();
    wait_for_mpris(&conn);

    assert_eq!(
        String::try_from(get(&conn, ROOT_IFACE, "Identity")).expect("Identity is a string"),
        "Soul Player",
        "the desktop should see the product name, not a binary name"
    );

    // Autoplay has to reach the sink before metadata is meaningful.
    wait_for("playback to start", || {
        (playback_status(&conn) == "Playing").then_some(())
    });

    assert_eq!(metadata_str(&conn, "xesam:title").as_deref(), Some("Aria"));
    assert_eq!(
        metadata_str(&conn, "xesam:artist").as_deref(),
        Some("Kimiko Ishizaka")
    );
    assert_eq!(
        metadata_str(&conn, "xesam:album").as_deref(),
        Some("Open Goldberg Variations")
    );

    assert!(app.is_running(), "the app should still be alive");
}

/// Artwork is published as a URL, so the only useful assertion is that the URL
/// RESOLVES. A path to a file that was never written (or was already cleaned
/// up) still reads as a perfectly good `mpris:artUrl` over the bus and shows a
/// blank square on the lock screen.
///
/// The staging directory must also stay BOUNDED at the one current cover. It
/// lives in the user's cache directory and is deliberately a cache (it survives
/// the process, so a relaunch does not re-decode art), but a player that left
/// one file per track played would quietly grow a junk pile there,
/// `publish_artwork` prunes every other `cover-*` as it writes. Asserting "one
/// file, and it is the one the bus advertised" is the guarantee the
/// implementation actually makes; asserting an empty directory after exit would
/// be testing a cleanup contract that does not exist.
#[test]
#[ignore = "needs a D-Bus session bus and a display; see the module docs"]
fn publishes_cover_art_that_resolves_and_stays_bounded() {
    let library = staged_library("arted.flac", "flac", 0.2);
    let mut app = App::launch(library.path(), true);
    let conn = session_bus();
    wait_for_mpris(&conn);
    wait_for("playback to start", || {
        (playback_status(&conn) == "Playing").then_some(())
    });

    wait_for("cover art to be published", || {
        metadata_str(&conn, "mpris:artUrl").filter(|u| !u.is_empty())
    });

    // PAUSE before inspecting the staged file. These fixtures are 0.2 s each, so
    // a playing deck changes track several times a second, and `publish_artwork`
    // prunes the previous cover as it writes the next, making "read artUrl,
    // then stat it" a race the test loses whenever a boundary falls between the
    // two. It did, in CI: `artUrl points at a file that does not exist:
    // …/cover-0.png`. Paused, the advertised URL and the file on disk stay
    // consistent, which is the property actually being asserted. (The same
    // reasoning as `set_position_seeks_within_the_track`, which pauses so its
    // read is deterministic.)
    call(&conn, "Pause");
    wait_for("Pause to take", || {
        (playback_status(&conn) == "Paused").then_some(())
    });

    // Re-read AFTER pausing: the URL captured above may name a track the deck
    // has already left behind.
    let url = wait_for("a cover whose file is on disk", || {
        let u = metadata_str(&conn, "mpris:artUrl").filter(|u| !u.is_empty())?;
        let p = percent_decode(u.strip_prefix("file://")?);
        Path::new(&p).is_file().then_some(u)
    });
    let path = percent_decode(
        url.strip_prefix("file://")
            .unwrap_or_else(|| panic!("MPRIS requires a file:// URL for local art, got {url:?}")),
    );
    let staged = Path::new(&path);
    assert!(
        staged.is_file(),
        "artUrl points at a file that does not exist: {url}"
    );
    assert!(
        std::fs::metadata(staged)
            .expect("stat the staged cover")
            .len()
            > 0,
        "the staged cover is empty"
    );

    let dir = staged
        .parent()
        .expect("the cover has a parent dir")
        .to_owned();
    assert!(
        app.signal_and_wait("TERM"),
        "the app should exit on SIGTERM"
    );
    let covers: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("cover-"))
                .collect()
        })
        .unwrap_or_default();
    // Count, not name: `staged_library` stages enough copies to play for
    // `PLAYING_SECONDS`, so the player legitimately advances past the track whose
    // artUrl was captured above, and the one surviving cover is a LATER track's.
    // Pinning the name would assert "the player never advanced", which is not the
    // property under test. One file is.
    assert_eq!(
        covers.len(),
        1,
        "the staging dir must hold exactly one cover, more means a per-track \
         leak, none means the current cover was pruned too eagerly; found {covers:?}"
    );
}

/// The transport the OS drives: every control a media key or lock-screen button
/// sends must move the player, and the status it reports back must be the one
/// MPRIS defines.
///
/// `Stop` is the pointed case, it used to report `Paused`, because the app
/// collapsed the backend's three-way play state into a bool and inferred
/// "stopped" from having no track loaded, which a Stop doesn't change.
#[test]
#[ignore = "needs a D-Bus session bus and a display; see the module docs"]
fn os_transport_controls_drive_playback() {
    let library = staged_library("meta.flac", "flac", 1.5);
    let _app = App::launch(library.path(), true);
    let conn = session_bus();
    wait_for_mpris(&conn);
    wait_for("playback to start", || {
        (playback_status(&conn) == "Playing").then_some(())
    });

    call(&conn, "Pause");
    wait_for("Pause to take", || {
        (playback_status(&conn) == "Paused").then_some(())
    });

    call(&conn, "Play");
    wait_for("Play to resume", || {
        (playback_status(&conn) == "Playing").then_some(())
    });

    call(&conn, "Stop");
    wait_for("Stop to report the MPRIS Stopped status", || {
        (playback_status(&conn) == "Stopped").then_some(())
    });
}

/// A library holding ONE long silent WAV, `seconds` long.
///
/// For timing assertions the committed fixtures are unusable: they are ~1.5 s
/// each, the OS position only refreshes about once a second, and so every
/// measurement window spans a track boundary where most of a track's played
/// time is lost between samples. One long track removes the boundary entirely.
///
/// Synthesised rather than committed, a minute of PCM is megabytes, and the
/// content is irrelevant here (this measures the clock, not the audio). Silence
/// also keeps a developer running these locally from being blasted by a tone.
fn staged_long_track(seconds: u32) -> tempfile::TempDir {
    staged_long_tracks(seconds, 1)
}

/// `staged_long_track` with `count` identical tracks, for a test that needs
/// somewhere to advance TO without reintroducing short-track boundaries.
fn staged_long_tracks(seconds: u32, count: usize) -> tempfile::TempDir {
    const RATE: u32 = 44_100;
    const CHANNELS: u16 = 2;
    const BITS: u16 = 16;

    let dir = tempfile::tempdir().expect("create a library temp dir");
    let album = dir.path().join("Timing");
    std::fs::create_dir_all(&album).expect("create the album dir");

    let bytes_per_frame = u32::from(CHANNELS) * u32::from(BITS) / 8;
    let data_len = RATE * seconds * bytes_per_frame;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // format: PCM
    wav.extend_from_slice(&CHANNELS.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * bytes_per_frame).to_le_bytes()); // byte rate
    wav.extend_from_slice(&(bytes_per_frame as u16).to_le_bytes()); // block align
    wav.extend_from_slice(&BITS.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(wav.len() + data_len as usize, 0);

    for i in 0..count {
        std::fs::write(album.join(format!("timing-{i:04}.wav")), &wav)
            .expect("write the timing fixture");
    }
    dir
}

/// Playback must advance at WALL-CLOCK rate.
///
/// This is the one property that makes it an audio player rather than a state
/// machine, and the failures it catches are invisible to every other test here:
/// a sink that opened but is starving reports `Playing` with a position that
/// barely moves, and a decode too slow to keep the audio clock fed (the risk
/// with DSD's 64–512x decimation) plays at the WRONG SPEED while still looking
/// healthy over the bus.
///
/// The tolerance is deliberately wide. A loaded CI runner can stall this
/// process between the two reads, and a flaky timing test would get muted,
/// which would cost more than the precision is worth. Anything inside it still
/// rules out "stalled" and "running at double speed".
#[test]
#[ignore = "needs a D-Bus session bus and a display; see the module docs"]
fn playback_advances_at_wall_clock_rate() {
    let library = staged_long_track(60);
    let _app = App::launch(library.path(), true);
    let conn = session_bus();
    wait_for_mpris(&conn);
    wait_for("playback to start", || {
        (playback_status(&conn) == "Playing").then_some(())
    });

    // One long track, so no boundary can swallow played time: the difference
    // between two readings IS the elapsed playback.
    let started = Instant::now();
    let first = position_us(&conn);
    std::thread::sleep(Duration::from_secs(3));
    let played = (position_us(&conn) - first) as f64 / 1e6;
    let wall = started.elapsed().as_secs_f64();

    // Wide bounds on purpose: the OS position refreshes about once a second, so
    // the reading quantises, and a loaded CI runner can stall this process
    // between samples. The window still rules out the failures that matter,
    // a stalled or starving sink, and playback running at the wrong speed.
    assert!(
        played > wall * 0.6,
        "position advanced {played:.2}s over {wall:.2}s of wall clock: \
         the sink is starving or playback stalled"
    );
    assert!(
        played < wall * 1.5,
        "position advanced {played:.2}s over {wall:.2}s of wall clock: \
         playback is running fast (a decode/sink rate mismatch)"
    );
}

/// `Next` is a real media key. It must move the queue on, observable as the
/// position restarting, since the staged copies are deliberately identical and
/// a metadata change would prove nothing.
#[test]
#[ignore = "needs a D-Bus session bus and a display; see the module docs"]
fn next_advances_the_queue() {
    // TWO LONG tracks, not the 1.5 s fixture copies. With short tracks the
    // position cycles 0 -> 1.5 s continuously, so "the position dropped below
    // 0.4 s" is indistinguishable from the track simply ending on its own, and
    // the window in which the reading is meaningful is a fraction of a second
    // wide. That ambiguity made this test the only flaky one in the suite (it
    // timed out once on main having passed on the branch). A 60 s track cannot
    // advance by itself within the test, so a restarted position can ONLY be
    // the Next we sent.
    let library = staged_long_tracks(60, 2);
    let _app = App::launch(library.path(), true);
    let conn = session_bus();
    wait_for_mpris(&conn);
    wait_for("playback to start", || {
        (playback_status(&conn) == "Playing").then_some(())
    });

    // Let the current track get far enough in that a restart is unambiguous.
    wait_for("the track to reach 0.4 s", || {
        (position_us(&conn) > 400_000).then_some(())
    });
    call(&conn, "Next");

    // Hand-rolled rather than `wait_for` so a timeout can SAY what the player
    // was doing. The one failure on record reported only "timed out", which was
    // not enough to tell a stalled deck from a player that had stopped.
    let deadline = Instant::now() + SETTLE;
    loop {
        let pos = position_us(&conn);
        if pos < 400_000 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Next did not restart the position on a new track within {SETTLE:?}; \
             status={}, position={pos} us",
            playback_status(&conn)
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    assert_eq!(
        playback_status(&conn),
        "Playing",
        "Next should keep playing, not stop the player"
    );
}

/// `SetPosition` is what a lock-screen scrubber sends. Asserted while PAUSED so
/// the read is deterministic: against a running clock the position moves
/// between the seek and the check, and the test would have to guess a window.
#[test]
#[ignore = "needs a D-Bus session bus and a display; see the module docs"]
fn set_position_seeks_within_the_track() {
    let library = staged_library("meta.flac", "flac", 1.5);
    let _app = App::launch(library.path(), true);
    let conn = session_bus();
    wait_for_mpris(&conn);
    wait_for("playback to start", || {
        (playback_status(&conn) == "Playing").then_some(())
    });
    call(&conn, "Pause");
    wait_for("Pause to take", || {
        (playback_status(&conn) == "Paused").then_some(())
    });

    // Fixtures are ~1.5 s, so 1.0 s is inside the track. The track id is the
    // one souvlaki publishes ("/"); a mismatch makes MPRIS ignore the call as
    // stale, which would look like a silently broken seek.
    let target_us: i64 = 1_000_000;
    let track = ObjectPath::try_from("/").expect("a valid object path");
    conn.call_method(
        Some(bus().as_str()),
        OBJECT,
        Some(PLAYER_IFACE),
        "SetPosition",
        &(&track, target_us),
    )
    .expect("SetPosition");

    let landed = wait_for("the seek to land", || {
        let p = position_us(&conn);
        ((p - target_us).abs() < 400_000).then_some(p)
    });
    assert!(
        (landed - target_us).abs() < 400_000,
        "sought to {target_us}us, landed at {landed}us"
    );
}

/// The properties every MPRIS consumer reads before it will show a player at
/// all. A desktop that finds `CanControl` false, or a missing `Identity`, hides
/// the player outright, so these are worth pinning even though nothing in the
/// app sets them directly: they come from the media-session layer, and a
/// dependency bump could change them silently.
///
/// NOT asserted: `DesktopEntry`. MPRIS marks it optional and `souvlaki` does
/// not publish it, which is why a GNOME media notification still shows a
/// generic icon rather than the app's, GNOME resolves that icon through
/// exactly this property. Fixing it needs a change in `souvlaki`, which owns
/// the interface; the value it should carry is already settled, since it is the
/// same `.desktop` basename the window's `app_id` uses.
#[test]
#[ignore = "needs a D-Bus session bus and a display; see the module docs"]
fn exposes_the_mpris_properties_a_desktop_requires() {
    let library = staged_library("meta.flac", "flac", 1.5);
    let _app = App::launch(library.path(), false);
    let conn = session_bus();
    wait_for_mpris(&conn);

    assert_eq!(
        String::try_from(get(&conn, ROOT_IFACE, "Identity")).expect("Identity is a string"),
        "Soul Player"
    );
    for prop in ["CanQuit", "CanRaise"] {
        assert!(
            bool::try_from(get(&conn, ROOT_IFACE, prop)).expect("a bool"),
            "{prop} should be true, the app can be quit and raised from the OS"
        );
    }
    for prop in [
        "CanControl",
        "CanPlay",
        "CanPause",
        "CanSeek",
        "CanGoNext",
        "CanGoPrevious",
    ] {
        assert!(
            bool::try_from(get(&conn, PLAYER_IFACE, prop)).expect("a bool"),
            "{prop} should be true, or a desktop will grey out that control"
        );
    }
    // Before anything plays the player is stopped, not an empty string or a
    // status outside the three MPRIS defines.
    assert_eq!(playback_status(&conn), "Stopped");
}

/// The window must advertise an `app_id` to the compositor.
///
/// This is how a Wayland desktop resolves a window to its `.desktop` entry, and
/// therefore to its icon: with the field empty, which is what the app shipped
/// before `rune::set_app_id` was called, the compositor reports a blank class,
/// the dock and task switcher fall back to a generic icon, and window rules
/// cannot target the app at all.
///
/// Nothing in-process can observe this (it is set on the window winit creates,
/// and read by the compositor), so the assertion has to come from the
/// compositor itself. Under sway that is `swaymsg -t get_tree`; CI's headless
/// Wayland job runs the suite inside exactly that.
///
/// The value must equal the basename of the shipped desktop entry, which
/// cargo-packager names after the main binary, so it is compared against the
/// binary name rather than a hardcoded string the two could drift apart on.
#[test]
#[ignore = "needs a running sway session (SWAYSOCK); CI's Wayland job provides one"]
fn window_advertises_its_app_id_to_the_compositor() {
    // Skipped rather than failed off-sway: the assertion needs sway's IPC, and a
    // developer on GNOME/Hyprland running `--ignored` has a missing tool, not a
    // regression. CI's headless Wayland job always has it, so coverage is not
    // optional there, same convention as the real-device hardware e2e.
    if std::env::var_os("SWAYSOCK").is_none() {
        eprintln!("no SWAYSOCK (not a sway session), skipping the app_id check");
        return;
    }
    let library = staged_library("meta.flac", "flac", 1.5);
    let _app = App::launch(library.path(), false);

    // The binary's own file stem, taken from the path cargo built, the same
    // thing `main` passes to `rune::set_app_id`, and the same thing
    // cargo-packager names the `.desktop` entry after. Derived rather than
    // written out, so a rename can't leave this test asserting a stale id.
    let expected = Path::new(env!("CARGO_BIN_EXE_soul-player"))
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("the built binary has a name");
    wait_for("the window to appear with its app_id", || {
        let out = Command::new("swaymsg")
            .args(["-t", "get_tree", "-r"])
            .output()
            .ok()?;
        let tree: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        find_app_id(&tree, expected).then_some(())
    });
}

/// The MPRIS bus name and the window's `app_id` must identify the app the SAME
/// way, because that is the only thing tying a media applet to the player's icon.
///
/// MPRIS has a `DesktopEntry` root property that would name the `.desktop` file
/// outright, but souvlaki does not implement it. With it absent, a desktop
/// resolves the player by matching the bus-name suffix against installed desktop
/// entries, so the suffix, the entry's basename and the `app_id` all have to be
/// the same string. They were not: the bus said `soul_player` while the entry and
/// `app_id` said `soul-player`, and a media applet had nothing to match.
///
/// Both halves are read back from the SYSTEM, the bus name from the session bus,
/// the `app_id` from the compositor's own tree, rather than compared against the
/// constants this crate defines, which would pass no matter what the app did.
#[test]
#[ignore = "needs a running sway session (SWAYSOCK); CI's Wayland job provides one"]
fn the_mpris_bus_name_and_the_window_app_id_agree() {
    if std::env::var_os("SWAYSOCK").is_none() {
        eprintln!("no SWAYSOCK (not a sway session), skipping the identity check");
        return;
    }
    let library = staged_library("meta.flac", "flac", 1.5);
    let _app = App::launch(library.path(), false);
    let conn = session_bus();
    wait_for_mpris(&conn);

    // The suffix the app actually registered on the bus: ask the bus for its
    // owned names rather than assuming the registration took.
    let owned: Vec<String> = zbus::blocking::fdo::DBusProxy::new(&conn)
        .expect("connect to the bus daemon")
        .list_names()
        .expect("list bus names")
        .into_iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
        .collect();
    let suffix = owned
        .iter()
        .find_map(|n| n.strip_prefix("org.mpris.MediaPlayer2."))
        .unwrap_or_else(|| panic!("no MPRIS player on the bus; saw {owned:?}"));

    // `wait_for` panics on timeout, so reaching the end IS the assertion: the
    // compositor reported a window whose app_id equals the bus suffix.
    wait_for("a window whose app_id matches the MPRIS bus suffix", || {
        let out = Command::new("swaymsg")
            .args(["-t", "get_tree", "-r"])
            .output()
            .ok()?;
        let tree: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        find_app_id(&tree, suffix).then_some(())
    });
}

/// Depth-first search of a sway tree for a node whose `app_id` is `wanted`.
/// Only a native Wayland window has one, an Xwayland window reports `class`
/// instead, so finding it here also proves the app did not quietly fall back
/// to X11.
fn find_app_id(node: &serde_json::Value, wanted: &str) -> bool {
    if node.get("app_id").and_then(|v| v.as_str()) == Some(wanted) {
        return true;
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(|v| v.as_array())
            && children.iter().any(|c| find_app_id(c, wanted))
        {
            return true;
        }
    }
    false
}

/// A desktop session ends by SIGTERMing its clients. The app must treat that as
/// an ordinary quit and clear its running-session sentinel, otherwise every
/// logout leaves one behind and the next launch opens with a crash-report
/// prompt over the window.
#[test]
#[ignore = "needs a D-Bus session bus and a display; see the module docs"]
fn sigterm_exits_cleanly_so_the_next_launch_reports_no_crash() {
    let library = staged_library("meta.flac", "flac", 1.5);
    let mut app = App::launch(library.path(), false);
    let conn = session_bus();
    wait_for_mpris(&conn);

    let sentinel = app.sentinel();
    assert!(
        sentinel.is_file(),
        "a running app should hold its session sentinel at {}",
        sentinel.display()
    );

    assert!(
        app.signal_and_wait("TERM"),
        "SIGTERM must terminate the app (it installs a handler, which SUPPRESSES \
         the default kill: so a missing clean-quit path hangs instead)"
    );
    assert!(
        !sentinel.is_file(),
        "the sentinel survived SIGTERM, so the next launch would report a crash"
    );
}

/// Decode `%XX` escapes. The app percent-encodes the cover path (a home
/// directory can hold spaces or non-ASCII), so the URL has to be decoded before
/// it names a real file.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(byte) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}
