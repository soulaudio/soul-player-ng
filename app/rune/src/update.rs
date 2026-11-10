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

fn config() -> Result<Config> {
    Ok(Config {
        endpoints: vec![ENDPOINT.parse()?],
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
    match try_available() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[updater] check skipped: {e}");
            false
        }
    }
}

fn try_available() -> Result<bool> {
    let current = env!("CARGO_PKG_VERSION").parse()?;
    Ok(check_update(current, config()?)?.is_some())
}

/// Download + install the latest signed release, verify its signature against
/// [`PUBKEY`], then relaunch. Blocks; on success the running process is replaced,
/// so this returns only on error or when already up to date. Call it off the UI
/// thread.
pub fn install() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION").parse()?;
    if let Some(update) = check_update(current, config()?)? {
        update.download_and_install()?;
    }
    Ok(())
}
