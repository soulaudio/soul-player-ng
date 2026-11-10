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

/// Blocking check: is a newer signed release available? Any failure (offline, no
/// release yet, signature mismatch) resolves to `false` so the caller just
/// carries on — a failed update check must never stop the player from running.
pub fn available() -> bool {
    available_at(&endpoint())
}

fn available_at(endpoint: &str) -> bool {
    match try_available(endpoint) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[updater] check skipped: {e}");
            false
        }
    }
}

fn try_available(endpoint: &str) -> Result<bool> {
    let current = env!("CARGO_PKG_VERSION").parse()?;
    Ok(check_update(current, config(endpoint)?)?.is_some())
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
    fn available_true_when_endpoint_offers_newer_version() {
        // Leak the manifest string to get the 'static the server needs.
        let body: &'static str = Box::leak(manifest("99.0.0").into_boxed_str());
        assert!(
            available_at(&serve(body)),
            "99.0.0 > current version is an available update"
        );
    }

    #[test]
    fn available_false_when_endpoint_version_is_older() {
        let body: &'static str = Box::leak(manifest("0.0.1").into_boxed_str());
        assert!(
            !available_at(&serve(body)),
            "0.0.1 < current version is no update"
        );
    }

    #[test]
    fn available_false_when_endpoint_unreachable() {
        // Port 1 is effectively never listening: the fetch fails and the check
        // degrades to `false` rather than erroring out.
        assert!(!available_at("http://127.0.0.1:1/latest.json"));
    }
}
