//! Auto-update via `cargo-packager-updater`.
//!
//! The shipped installer is **OS-unsigned** (we don't pay for an Apple/Windows
//! signing identity yet), but updates are still cryptographically verified: every
//! downloaded build must carry a signature from our own updater key (the
//! `PUBKEY` below), which is wholly independent of OS code-signing. So an app the
//! OS treats as "unknown publisher" still auto-updates safely.
//!
//! Flow (owned by `main`/`views::settings`, not here, so this stays Rune-free):
//! on launch a background thread calls [`available`]; if a newer signed release
//! exists it flips a flag and Settings surfaces an "Install & Restart" button,
//! which calls [`install`]. We deliberately do **not** auto-install on launch —
//! yanking a music player out from under a playing track is worse than letting
//! the user pick the moment.
#![forbid(unsafe_code)]

use anyhow::Result;
use cargo_packager_updater::{Config, WindowsConfig, WindowsUpdateInstallMode, check_update};

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

/// Whether auto-update is active for this build. Release builds only, and never
/// when `SOUL_PLAYER_NO_UPDATE` is set (the e2e harness sets it, so tests neither
/// hit the network nor restart mid-run). Debug builds skip it too — there is no
/// matching published release to pull.
pub fn enabled() -> bool {
    !cfg!(debug_assertions) && std::env::var_os("SOUL_PLAYER_NO_UPDATE").is_none()
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

/// Blocking check: the version string of a newer signed release, or `None` when
/// we're up to date or the check failed (offline / no release yet / bad
/// manifest). Any failure resolves to `None` so the caller just carries on — a
/// failed update check must never stop the player from running.
pub fn available() -> Option<String> {
    available_at(&endpoint())
}

fn available_at(endpoint: &str) -> Option<String> {
    match try_available(endpoint) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[updater] check skipped: {e}");
            None
        }
    }
}

fn try_available(endpoint: &str) -> Result<Option<String>> {
    let current = env!("CARGO_PKG_VERSION").parse()?;
    Ok(check_update(current, config(endpoint)?)?.map(|u| u.version))
}

/// Download + install the latest signed release, verify its signature against
/// [`PUBKEY`], then relaunch. Blocks; on success the running process is replaced,
/// so this returns only on error or when already up to date. Call it off the UI
/// thread.
pub fn install() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION").parse()?;
    if let Some(update) = check_update(current, config(&endpoint())?)? {
        update.download_and_install()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

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
    fn available_returns_version_when_endpoint_offers_newer() {
        // Leak the manifest string to get the 'static the server needs.
        let body: &'static str = Box::leak(manifest("99.0.0").into_boxed_str());
        assert_eq!(
            available_at(&serve(body)).as_deref(),
            Some("99.0.0"),
            "99.0.0 > current version is an available update"
        );
    }

    #[test]
    fn available_none_when_endpoint_version_is_older() {
        let body: &'static str = Box::leak(manifest("0.0.1").into_boxed_str());
        assert!(
            available_at(&serve(body)).is_none(),
            "0.0.1 < current version is no update"
        );
    }

    #[test]
    fn available_none_when_endpoint_unreachable() {
        // Port 1 is effectively never listening: the fetch fails and the check
        // degrades to `None` rather than erroring out.
        assert!(available_at("http://127.0.0.1:1/latest.json").is_none());
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
}
