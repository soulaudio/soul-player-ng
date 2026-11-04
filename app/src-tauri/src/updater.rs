//! Auto-update wiring over `tauri-plugin-updater`.
//!
//! Shortly after launch a background task asks the GitHub release `latest.json`
//! whether a newer signed bundle exists for this platform. The bundle signature
//! is verified against the minisign public key baked into `tauri.conf.json`
//! before anything is applied, so a tampered release can't be installed. When an
//! update is available we emit an `update-available` event (version + notes) to
//! the frontend, which owns the user-facing prompt and triggers the install via
//! the `@tauri-apps/plugin-updater` JS API.
//!
//! The check is skipped when `SOUL_PLAYER_SKIP_UPDATE` is set (dev, CI, e2e) so
//! a test or offline run never reaches for the network.

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

/// Payload for the `update-available` event the frontend listens for.
#[derive(Clone, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

/// Spawn the post-startup update check unless disabled by env.
pub fn spawn_startup_check(app: &AppHandle) {
    if std::env::var_os("SOUL_PLAYER_SKIP_UPDATE").is_some() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match check(&app).await {
            Ok(Some(info)) => {
                eprintln!("[updater] update available: {}", info.version);
                // Inert until the frontend subscribes; the plumbing is what the
                // signed-release pipeline exists to feed.
                let _ = app.emit("update-available", info);
            }
            Ok(None) => {}
            Err(e) => eprintln!("[updater] check failed: {e}"),
        }
    });
}

/// Query the configured endpoint for a newer signed release.
async fn check(app: &AppHandle) -> tauri_plugin_updater::Result<Option<UpdateInfo>> {
    let update = app.updater()?.check().await?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version.clone(),
        notes: u.body.clone(),
        date: u.date.map(|d| d.to_string()),
    }))
}
