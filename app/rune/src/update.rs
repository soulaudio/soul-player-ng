//! Auto-update via `cargo-packager-updater`.
//!
//! The shipped installer is **OS-unsigned** (we don't pay for an Apple/Windows
//! signing identity yet), but updates are still cryptographically verified: every
//! downloaded build must carry a signature from our own updater key (the
//! `PUBKEY` below), which is wholly independent of OS code-signing. So an app the
//! OS treats as "unknown publisher" still auto-updates safely.
//!
//! The policy follows the established desktop updaters (Sparkle, VS Code, GitHub
//! Desktop): **check** automatically by default — at launch, then on a schedule
//! ([`DEFAULT_INTERVAL`], jittered, backing off after a failure) so a player left
//! open for days still learns about a release — but never yank a playing track
//! away. Installing is the user's call: "Install & Restart" now, or the opt-in
//! "Install updates automatically", which downloads in the background and installs
//! when the user next quits. Both switches live in Settings → Updates.
//!
//! This module stays Rune-free; `main` owns the scheduler thread and the UI state:
//! - [`check`] asks the manifest whether a newer signed release exists.
//! - [`download`] fetches and **verifies** it and stages it, either to relaunch
//!   ("Install & Restart": `main` then quits the app the normal way) or to install
//!   silently on quit.
//! - [`apply_pending`] runs once the run loop has returned (every quit hook has
//!   run, the session is marked clean) and puts the staged update in place.
//!
//! Environment (tests / managed machines): `SOUL_PLAYER_NO_UPDATE` turns it all
//! off, `SOUL_PLAYER_UPDATE_ENDPOINT` overrides the manifest URL,
//! `SOUL_PLAYER_UPDATE_INTERVAL_SECS` the schedule, and
//! `SOUL_PLAYER_AUTO_INSTALL_UPDATE` installs a found update at once and restarts
//! ([`install_immediately`]) — how the release e2e drives real updates.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use cargo_packager_updater::{
    Config, Update, WindowsConfig, WindowsUpdateInstallMode, check_update,
};

/// Updater public key — pairs with the private key in `.signing/` (held as a CI
/// secret, never committed). Every downloaded update is verified against this
/// before it is applied. Public by nature; safe to embed.
const PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDMwNEU3QTMzMDMxQUZCODUKUldTRit4b0RNM3BPTUFhMEw4ZFJuRHpGN1RGYklVaXF6QTJlNTFxeVZENlJXNW5qaUcrQ3REbmkK";

/// GitHub Releases is the static update backend: `latest/download/<asset>`
/// always resolves to the newest published release, so this URL never changes
/// across versions. The release CI publishes `latest.json` (the update manifest)
/// as a release asset.
const ENDPOINT: &str =
    "https://github.com/soulaudio/soul-player-ng/releases/latest/download/latest.json";

/// How often a running app re-checks. Sparkle's default is a day; a music player
/// is often left open for days, so twice a day, which is still one small static
/// file from a CDN per user.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);

/// Floor for the schedule (Sparkle enforces an hour too), so a mistyped override
/// can't hammer the server. The env override for tests may go lower.
const MIN_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// After a failed check (offline, GitHub hiccup), retry sooner than the full
/// interval — but not in a tight loop.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(30 * 60);

/// Whether auto-update is active for this build. Release builds only, and never
/// when `SOUL_PLAYER_NO_UPDATE` is set (the e2e harness sets it, so tests neither
/// hit the network nor restart mid-run). Debug builds skip it too — there is no
/// matching published release to pull.
///
/// It is also off wherever a PACKAGE MANAGER owns the install — see
/// [`manager_owned`]. The updater works by downloading a new build and swapping
/// the running binary for it, which is right for an AppImage the user dropped
/// in `~/Applications` and wrong, or outright impossible, for anything else.
pub fn enabled() -> bool {
    !cfg!(debug_assertions)
        && std::env::var_os("SOUL_PLAYER_NO_UPDATE").is_none()
        && !manager_owned(
            crate::in_flatpak(),
            std::env::var_os("APPIMAGE").is_some(),
            std::env::current_exe().ok().as_deref(),
        )
        && !needs_move()
}

/// macOS: the app runs from a disk image or a translocated copy, where the
/// bundle can't be replaced — downloading an update there would only fail at
/// the install. Moving it to Applications (`app_location`) fixes it, and the
/// Settings page says so.
pub fn needs_move() -> bool {
    static NEEDS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NEEDS
        .get_or_init(|| crate::app_location::current().is_some_and(|(_, place)| !place.updatable()))
}

/// Whether something other than this app is responsible for updating it.
///
/// Three cases, and the order matters:
///
/// * **Flatpak** — the app is installed read-only from a remote the user chose,
///   `flatpak update` replaces it, and the sandbox would refuse the write in any
///   case.
/// * **AppImage** — NOT manager-owned: this is the one layout the updater is
///   built for, and it is checked BEFORE the path test below because a running
///   AppImage mounts itself at `/tmp/.mount_XXXX/usr/bin/…`, which contains
///   `/usr` and would otherwise be mistaken for a system install. The runtime
///   sets `APPIMAGE` to the image's own path, which is what distinguishes them.
/// * **A system path** — a `.deb` or `.rpm` installs to `/usr/bin`, owned by
///   root. A user-run app cannot replace that file, so attempting it would
///   download a few tens of megabytes and then fail on a permission error,
///   which is a worse outcome than not offering it: `apt upgrade` /
///   `dnf upgrade` is the real update path there.
///
/// Taking its inputs as arguments rather than reading the environment directly
/// keeps it testable — `std::env::set_var` is `unsafe` as of the 2024 edition
/// and races other threads.
fn manager_owned(in_flatpak: bool, is_appimage: bool, exe: Option<&std::path::Path>) -> bool {
    if in_flatpak {
        return true;
    }
    if is_appimage {
        return false;
    }
    exe.is_some_and(|p| p.starts_with("/usr") || p.starts_with("/opt"))
}

/// Whether something else is responsible for updating this install — a package
/// manager or Flatpak, rather than the app itself.
///
/// [`enabled`] already refuses to update in that case; this exists so the
/// Settings page can say WHICH case it is. "Updates are turned off for this
/// build" is true of a debug build and misleading on a .deb: that user's
/// updates are not off, they arrive through `apt`. Telling them otherwise
/// invites a bug report about an update button that is working as designed.
pub fn managed_elsewhere() -> bool {
    manager_owned(
        crate::in_flatpak(),
        std::env::var_os("APPIMAGE").is_some(),
        std::env::current_exe().ok().as_deref(),
    )
}

/// The manifest URL to check. `SOUL_PLAYER_UPDATE_ENDPOINT` overrides it — handy
/// for a self-hosted update server, and how the tests point the updater at a
/// local one; otherwise the stable GitHub Releases [`ENDPOINT`].
fn endpoint() -> String {
    std::env::var("SOUL_PLAYER_UPDATE_ENDPOINT").unwrap_or_else(|_| ENDPOINT.to_string())
}

fn config(endpoint: &str) -> Result<Config> {
    Ok(Config {
        endpoints: vec![endpoint.parse()?],
        pubkey: PUBKEY.into(),
        // Passive: a progress bar, no clicks — the update applies and relaunches
        // without interrupting the user for confirmation (they already opted in).
        windows: Some(WindowsConfig {
            install_mode: Some(WindowsUpdateInstallMode::Passive),
            ..Default::default()
        }),
    })
}

// ── Settings (mirrored from config by `main`, read by the scheduler) ─────────

static AUTO_CHECK: AtomicBool = AtomicBool::new(true);
static AUTO_INSTALL: AtomicBool = AtomicBool::new(false);

/// Settings → Updates → "Check for updates automatically" (default on).
pub fn auto_check() -> bool {
    AUTO_CHECK.load(Ordering::Relaxed)
}

pub fn set_auto_check(on: bool) {
    AUTO_CHECK.store(on, Ordering::Relaxed);
}

/// Settings → Updates → "Install updates automatically" (default off): download
/// a found update in the background and install it when the app is next quit.
pub fn auto_install() -> bool {
    AUTO_INSTALL.load(Ordering::Relaxed)
}

pub fn set_auto_install(on: bool) {
    AUTO_INSTALL.store(on, Ordering::Relaxed);
}

/// Install a found update at once and restart, unattended. Off unless
/// `SOUL_PLAYER_AUTO_INSTALL_UPDATE` is set (the release e2e sets it to drive a
/// real update; it also suits kiosk / managed machines).
pub fn install_immediately() -> bool {
    std::env::var_os("SOUL_PLAYER_AUTO_INSTALL_UPDATE").is_some()
}

// ── Schedule ─────────────────────────────────────────────────────────────────

/// The configured check interval: [`DEFAULT_INTERVAL`], or
/// `SOUL_PLAYER_UPDATE_INTERVAL_SECS` (tests use a few seconds).
pub fn interval() -> Duration {
    std::env::var("SOUL_PLAYER_UPDATE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_INTERVAL)
}

/// How long to wait before the next scheduled check. `jitter` ∈ [0, 1) spreads
/// clients over ±10 % of the interval so a fleet started together doesn't check in
/// lockstep. A failed check retries after [`RETRY_AFTER_FAILURE`] (or the
/// interval, if that is shorter). Intervals below [`MIN_INTERVAL`] are honoured
/// only when they came from the test override (`floor = false`).
pub fn next_delay(interval: Duration, last_failed: bool, jitter: f64, floor: bool) -> Duration {
    let base = if floor {
        interval.max(MIN_INTERVAL)
    } else {
        interval
    };
    let base = if last_failed {
        base.min(RETRY_AFTER_FAILURE)
    } else {
        base
    };
    let spread = base.as_secs_f64() * 0.1;
    Duration::from_secs_f64(
        (base.as_secs_f64() + spread * (2.0 * jitter.clamp(0.0, 1.0) - 1.0)).max(1.0),
    )
}

/// Whether the interval came from the test override (and so skips the floor).
pub fn interval_overridden() -> bool {
    std::env::var_os("SOUL_PLAYER_UPDATE_INTERVAL_SECS").is_some()
}

/// A cheap jitter source in [0, 1) — the clock's sub-second part. Good enough to
/// de-synchronise clients; nothing here needs real randomness.
pub fn jitter() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| f64::from(d.subsec_nanos()) / 1e9)
        .unwrap_or(0.5)
}

// ── Check ────────────────────────────────────────────────────────────────────

/// A newer signed release the manifest offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub version: String,
    /// The manifest's release notes, when it has any.
    pub notes: Option<String>,
}

static CHECKING: AtomicBool = AtomicBool::new(false);

/// Claim the single check slot (a manual "Check now" racing the schedule runs
/// once). Pair with [`end_check`].
pub fn begin_check() -> bool {
    !CHECKING.swap(true, Ordering::AcqRel)
}

pub fn end_check() {
    CHECKING.store(false, Ordering::Release);
}

/// Blocking check: the newer signed release, `None` when up to date, or the
/// error (offline / no release yet / bad manifest) for the caller to show or
/// retry. Never panics, never stops the player.
pub fn check() -> Result<Option<Found>> {
    check_at(&endpoint())
}

fn check_at(endpoint: &str) -> Result<Option<Found>> {
    let current = env!("CARGO_PKG_VERSION").parse()?;
    Ok(check_update(current, config(endpoint)?)?.map(|u| Found {
        version: u.version,
        notes: u.body.filter(|b| !b.trim().is_empty()),
    }))
}

// ── Download + install ───────────────────────────────────────────────────────

/// A downloaded, signature-verified update, held until the app has quit cleanly.
struct Staged {
    update: Update,
    bytes: Vec<u8>,
    /// Restart into the new build after installing ("Install & Restart"), or just
    /// install and stay closed (installed on quit).
    relaunch: bool,
}

static STAGED: Mutex<Option<Staged>> = Mutex::new(None);

fn staged() -> std::sync::MutexGuard<'static, Option<Staged>> {
    STAGED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Check for, download and **verify** (against [`PUBKEY`]) the latest signed
/// release, and stage it for [`apply_pending`]. Returns its version, or `None`
/// when already up to date. Blocks on the network: call it off the UI thread.
/// `relaunch`: restart into it after installing (then quit the app), or install it
/// silently whenever the app is next quit.
pub fn download(relaunch: bool) -> Result<Option<String>> {
    download_from(&endpoint(), relaunch)
}

fn download_from(endpoint: &str, relaunch: bool) -> Result<Option<String>> {
    let current = env!("CARGO_PKG_VERSION").parse()?;
    let Some(update) = check_update(current, config(endpoint)?)? else {
        return Ok(None);
    };
    let bytes = update.download()?;
    let version = update.version.clone();
    *staged() = Some(Staged {
        update,
        bytes,
        relaunch,
    });
    Ok(Some(version))
}

/// The version already downloaded and waiting, if any.
pub fn staged_version() -> Option<String> {
    staged().as_ref().map(|s| s.update.version.clone())
}

/// Turn an install-on-quit download into "Install & Restart" (no re-download).
/// False when nothing is staged.
pub fn stage_relaunch() -> bool {
    staged().as_mut().map(|s| s.relaunch = true).is_some()
}

/// Drop a staged update (the user turned "Install updates automatically" off).
pub fn discard_staged() {
    staged().take();
}

/// Install the update [`download`] staged, if any. Call once the run loop has
/// returned (the quit hooks have run). Windows hands over to the NSIS installer
/// (passive: a progress bar, no clicks) and exits this process; macOS and Linux
/// replace the bundle / AppImage in place. With `relaunch` the new build is then
/// started (on Windows by the installer's `/R`).
pub fn apply_pending() {
    let Some(Staged {
        mut update,
        bytes,
        relaunch,
    }) = staged().take()
    else {
        return;
    };
    tracing::info!(version = %update.version, relaunch, "installing update");
    if !relaunch {
        // Passive without `/R`: the installer must not start the app the user
        // just quit. (`BasicUi` contributes no NSIS flags; `/P` is passive.)
        update.config.windows = Some(WindowsConfig {
            install_mode: Some(WindowsUpdateInstallMode::BasicUi),
            installer_args: Some(vec!["/P".into()]),
        });
    }
    if let Err(e) = update.install(bytes) {
        tracing::error!("update install failed: {e}");
        return;
    }
    if !relaunch {
        tracing::info!("update installed; it starts next time");
        return;
    }
    match relaunch_target().map(|exe| std::process::Command::new(&exe).spawn()) {
        Some(Ok(child)) => tracing::info!(pid = child.id(), "started the updated app"),
        Some(Err(e)) => tracing::error!("update installed, but restarting failed: {e}"),
        None => tracing::error!("update installed, but the app's path is unknown"),
    }
}

/// What to start after an in-place update: the AppImage file itself on Linux
/// (`current_exe` is inside its now-stale mount), else this executable's path,
/// which now holds the new build (the macOS updater swaps the whole bundle).
fn relaunch_target() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// Who owns the install decides whether the updater may run at all, and the
    /// AppImage case is the trap: a running AppImage mounts itself under
    /// `/tmp/.mount_XXXX/usr/bin/`, so a naive "is it under /usr" test would
    /// classify the one layout the updater IS for as manager-owned and silently
    /// disable self-update for every AppImage user.
    #[test]
    fn package_manager_owned_installs_do_not_self_update() {
        use std::path::Path;

        // AppImage: the updater's home ground. The mount path contains /usr, so
        // this only passes because `APPIMAGE` is checked first.
        assert!(
            !manager_owned(
                false,
                true,
                Some(Path::new("/tmp/.mount_soulpl123/usr/bin/soul-player-rune"))
            ),
            "an AppImage must keep self-update despite its /usr-shaped mount path"
        );

        // .deb / .rpm: /usr/bin is root-owned, so an install would fail on
        // permissions after downloading tens of megabytes.
        assert!(manager_owned(
            false,
            false,
            Some(Path::new("/usr/bin/soul-player-rune"))
        ));
        assert!(manager_owned(
            false,
            false,
            Some(Path::new("/opt/soul-player/soul-player-rune"))
        ));

        // Flatpak: read-only install, sandboxed; `flatpak update` owns it. The
        // flag wins regardless of path.
        assert!(manager_owned(
            true,
            false,
            Some(Path::new("/app/bin/soul-player-rune"))
        ));

        // A loose build the user put somewhere writable keeps self-update.
        assert!(!manager_owned(
            false,
            false,
            Some(Path::new("/home/me/Applications/soul-player-rune"))
        ));
        // Unknown path: assume self-update works rather than silently disabling
        // it — the install can still fail loudly, which is more debuggable.
        assert!(!manager_owned(false, false, None));
    }

    // A tiny stand-in "installer" and its real minisign signature (made with the
    // updater key, committed). The .sig verifies against PUBKEY, so download()
    // exercises the true crypto path without a real 8 MB bundle.
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/update-fixture.bin");
    const FIXTURE_SIG: &str = include_str!("../tests/fixtures/update-fixture.bin.sig");

    /// A throwaway HTTP server that answers every request with `body`, for
    /// pointing the updater at a manifest we control. Returns its URL; the
    /// listener thread is detached and dies with the test process.
    fn serve(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}/latest.json")
    }

    // All three platform keys, so the check matches whatever host runs the test
    // (the updater looks up its own `{os}-{arch}`).
    fn manifest(version: &str) -> String {
        format!(
            r#"{{"version":"{version}","pub_date":"2026-01-01T00:00:00Z","notes":"t","platforms":{{"windows-x86_64":{{"url":"http://127.0.0.1/x","signature":"s","format":"nsis"}},"macos-aarch64":{{"url":"http://127.0.0.1/x","signature":"s","format":"app"}},"linux-x86_64":{{"url":"http://127.0.0.1/x","signature":"s","format":"appimage"}}}}}}"#
        )
    }

    #[test]
    fn check_returns_version_when_endpoint_offers_newer() {
        // Leak the manifest string to get the 'static the server needs.
        let body: &'static str = Box::leak(manifest("99.0.0").into_boxed_str());
        assert_eq!(
            check_at(&serve(body))
                .unwrap()
                .map(|f| f.version)
                .as_deref(),
            Some("99.0.0"),
            "99.0.0 > current version is an available update"
        );
    }

    #[test]
    fn check_carries_the_release_notes() {
        let body: &'static str = Box::leak(manifest("99.0.0").into_boxed_str());
        let found = check_at(&serve(body)).unwrap().expect("newer");
        assert_eq!(
            found.notes.as_deref(),
            Some("t"),
            "manifest `notes` → Found::notes"
        );
    }

    #[test]
    fn schedule_jitters_around_the_interval_and_retries_sooner_after_a_failure() {
        let day = Duration::from_secs(86_400);
        // jitter 0.5 = exactly the interval; 0 / 1 = −10 % / +10 %.
        assert_eq!(next_delay(day, false, 0.5, true), day);
        assert_eq!(next_delay(day, false, 0.0, true).as_secs(), 77_760);
        assert_eq!(next_delay(day, false, 1.0, true).as_secs(), 95_040);
        // A failure retries after ~30 min, not a whole day …
        let retry = next_delay(day, true, 0.5, true);
        assert_eq!(retry, RETRY_AFTER_FAILURE);
        // … and never below the hour floor in production …
        assert_eq!(
            next_delay(Duration::from_secs(5), false, 0.5, true),
            MIN_INTERVAL
        );
        // … while the test override may go as low as it likes.
        assert_eq!(
            next_delay(Duration::from_secs(5), false, 0.5, false).as_secs(),
            5
        );
        assert_eq!(
            next_delay(Duration::from_secs(5), true, 0.5, false).as_secs(),
            5
        );
        assert!(DEFAULT_INTERVAL >= MIN_INTERVAL);
    }

    #[test]
    fn only_one_check_runs_at_a_time() {
        assert!(begin_check(), "the slot is free");
        assert!(!begin_check(), "a second check while one runs is refused");
        end_check();
        assert!(begin_check(), "free again once the first ends");
        end_check();
    }

    #[test]
    fn check_none_when_endpoint_version_is_older() {
        let body: &'static str = Box::leak(manifest("0.0.1").into_boxed_str());
        assert!(
            check_at(&serve(body)).unwrap().is_none(),
            "0.0.1 < current version is no update"
        );
    }

    #[test]
    fn check_errors_when_endpoint_unreachable() {
        // Port 1 is effectively never listening: the fetch fails and the error is
        // returned (Settings shows it; the scheduler retries sooner).
        assert!(check_at("http://127.0.0.1:1/latest.json").is_err());
    }

    /// Two-route server for the full download path: `/latest.json` returns a
    /// manifest whose every platform points `/artifact` at `artifact` (served
    /// verbatim) with `sig`. Returns the manifest URL.
    fn serve_update(artifact: Vec<u8>, sig: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap();
        let manifest = format!(
            r#"{{"version":"99.0.0","pub_date":"2026-01-01T00:00:00Z","notes":"e2e","platforms":{{"windows-x86_64":{{"url":"http://{addr}/artifact","signature":"{sig}","format":"nsis"}},"macos-aarch64":{{"url":"http://{addr}/artifact","signature":"{sig}","format":"app"}},"linux-x86_64":{{"url":"http://{addr}/artifact","signature":"{sig}","format":"appimage"}}}}}}"#
        );
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (ctype, body) = if path.starts_with("/artifact") {
                    ("application/octet-stream", artifact.clone())
                } else {
                    ("application/json", manifest.clone().into_bytes())
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        format!("http://{addr}/latest.json")
    }

    /// End to end: check → download → **verify the real signature** against the
    /// embedded `PUBKEY`. Proves the generated key ↔ embedded pubkey loop works
    /// (short of the process-replacing install). Stops at `download()`, which
    /// does the verification; `install()` is the only untested step.
    #[test]
    fn download_verifies_the_real_signature_end_to_end() {
        let endpoint = serve_update(FIXTURE.to_vec(), FIXTURE_SIG.trim());
        let current = env!("CARGO_PKG_VERSION").parse().unwrap();
        let update = check_update(current, config(&endpoint).unwrap())
            .unwrap()
            .expect("99.0.0 is newer than the test binary");
        let bytes = update
            .download()
            .expect("the real .sig verifies against the embedded PUBKEY");
        assert_eq!(
            bytes, FIXTURE,
            "download returns the verified artifact bytes"
        );
    }

    #[test]
    fn download_rejects_a_tampered_artifact() {
        let mut tampered = FIXTURE.to_vec();
        tampered[0] ^= 0xFF; // flip a byte → the signature no longer matches
        let endpoint = serve_update(tampered, FIXTURE_SIG.trim());
        let current = env!("CARGO_PKG_VERSION").parse().unwrap();
        let update = check_update(current, config(&endpoint).unwrap())
            .unwrap()
            .expect("99.0.0 is newer than the test binary");
        assert!(
            update.download().is_err(),
            "a tampered artifact must fail signature verification"
        );
    }

    /// "Install & Restart" stages only a verified download, and only once: a tampered
    /// artifact stages nothing, a good one stages exactly one update for
    /// `apply_pending` to take. (One test, since `STAGED` is process-global.)
    #[test]
    fn download_stages_only_a_verified_update() {
        let take = || {
            STAGED
                .lock()
                .unwrap()
                .take()
                .map(|s| (s.update.version, s.bytes))
        };
        let mut tampered = FIXTURE.to_vec();
        tampered[0] ^= 0xFF;
        assert!(download_from(&serve_update(tampered, FIXTURE_SIG.trim()), true).is_err());
        assert!(take().is_none(), "a failed verification stages nothing");

        let version = download_from(&serve_update(FIXTURE.to_vec(), FIXTURE_SIG.trim()), false)
            .expect("verifies")
            .expect("99.0.0 is newer");
        assert_eq!(version, "99.0.0");
        assert_eq!(
            take(),
            Some(("99.0.0".to_string(), FIXTURE.to_vec())),
            "the verified bytes are staged"
        );
        assert!(take().is_none(), "staged once; applying takes it");

        // Install-on-quit download → "Install & Restart" upgrades it in place, no
        // re-download; switching auto-install off drops it.
        download_from(&serve_update(FIXTURE.to_vec(), FIXTURE_SIG.trim()), false).unwrap();
        assert_eq!(staged_version().as_deref(), Some("99.0.0"));
        assert!(!STAGED.lock().unwrap().as_ref().unwrap().relaunch);
        assert!(
            stage_relaunch(),
            "a staged update can be switched to relaunch"
        );
        assert!(STAGED.lock().unwrap().as_ref().unwrap().relaunch);
        discard_staged();
        assert_eq!(staged_version(), None);
        assert!(!stage_relaunch(), "nothing staged, nothing to switch");

        let older: &'static str = Box::leak(manifest("0.0.1").into_boxed_str());
        assert_eq!(
            download_from(&serve(older), true).unwrap(),
            None,
            "nothing newer, nothing staged"
        );
        assert!(take().is_none());
    }

    /// Live smoke test against the real published GitHub release: fetch the live
    /// manifest, download the real installer, and verify its signature against
    /// the embedded `PUBKEY`. Hits the network and pulls the full installer, so
    /// it is `#[ignore]`d — run after publishing a release:
    /// `cargo test -p soul-player-rune live_release -- --ignored`.
    #[test]
    #[ignore = "network: downloads the live GitHub release; run manually after a release"]
    fn live_release_downloads_and_verifies() {
        // Pretend to be an ancient build so the current release reads as newer.
        let current = "0.0.1".parse().unwrap();
        let update = check_update(current, config(ENDPOINT).unwrap())
            .unwrap()
            .expect("the live release should be newer than 0.0.1");
        let bytes = update
            .download()
            .expect("the live installer must verify against the embedded PUBKEY");
        assert!(
            bytes.len() > 1_000_000,
            "downloaded the real installer ({} bytes)",
            bytes.len()
        );
    }
}
