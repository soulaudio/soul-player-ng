//! The app side of auto-update: what Settings → Updates shows, the background
//! schedule, and the actions behind its buttons and switches. The network and
//! install mechanics are in [`crate::update`] (Rune-free); this module owns the
//! [`UpdateUi`] slice of `State` and the threads that feed it.
//!
//! Lifecycle of one release, from the app's point of view:
//!
//! ```text
//! Idle/UpToDate ─check─► Checking ─► Available ─"Install & Restart"─► Downloading ─► Restarting ─quit─► installed + relaunched
//!                                        │
//!                                        └─(Install automatically on)─► Downloading ─► Ready ─user quits─► installed, stays closed
//! ```
//!
//! A failed check shows the error and retries sooner (see `update::next_delay`);
//! it never interrupts playback, and an already-found update stays offered.
#![forbid(unsafe_code)]

use std::time::Instant;

use rune::{AppHandle, Tx};

use crate::update::{self, Found};
use crate::{State, config, notify};

/// Where the update flow stands, for Settings → Updates.
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) enum UpdateStatus {
    /// Nothing checked yet this session.
    #[default]
    Idle,
    Checking,
    /// The last check found nothing newer.
    UpToDate,
    /// A newer signed release is on offer.
    Available(Found),
    /// Downloading + verifying it.
    Downloading(Found),
    /// Downloaded and verified; installs when the app is next quit.
    Ready(Found),
    /// Downloaded and verified; the app is quitting to install it and restart.
    Restarting(String),
    /// The last check or download failed (the message is shown as is).
    Failed(String),
}

impl UpdateStatus {
    /// The release this status is about, if any.
    pub(crate) fn found(&self) -> Option<&Found> {
        match self {
            Self::Available(f) | Self::Downloading(f) | Self::Ready(f) => Some(f),
            _ => None,
        }
    }
}

/// The Updates slice of `State`.
#[derive(Clone, Debug)]
pub(crate) struct UpdateUi {
    pub(crate) status: UpdateStatus,
    /// When the last check finished (successfully or not).
    pub(crate) last_checked: Option<Instant>,
    /// Settings switch: check at launch and on a schedule.
    pub(crate) auto_check: bool,
    /// Settings switch: download in the background, install on quit.
    pub(crate) auto_install: bool,
    /// Whether this build updates at all (release build, no
    /// `SOUL_PLAYER_NO_UPDATE`). Off, the page says so instead of offering buttons.
    pub(crate) supported: bool,
    /// When `supported` is false, whether that is because a package manager
    /// owns this install (apt / dnf / flatpak) rather than because the build
    /// has updates compiled out. The page says different things.
    pub(crate) managed_elsewhere: bool,
    /// When `supported` is false, whether that is because the app runs from a
    /// disk image or a translocated copy (macOS) — moving it to Applications
    /// turns updates on.
    pub(crate) needs_move: bool,
    /// The version already announced with an OS notification this session, so
    /// the scheduled re-checks don't repeat it.
    notified: Option<String>,
}

impl UpdateUi {
    /// The persisted switches, nothing checked yet.
    pub(crate) fn load() -> Self {
        let (auto_check, auto_install) = config::load_update_prefs();
        Self {
            status: UpdateStatus::Idle,
            last_checked: None,
            auto_check,
            auto_install,
            supported: update::enabled(),
            managed_elsewhere: update::managed_elsewhere(),
            needs_move: update::needs_move(),
            notified: None,
        }
    }
}

/// Start the background schedule (release builds, unless `SOUL_PLAYER_NO_UPDATE`):
/// a check at launch, then every `update::interval()` (jittered; sooner after a
/// failure) while "Check for updates automatically" is on.
pub(crate) fn start(ah: AppHandle<State>) {
    let (auto_check, auto_install) = config::load_update_prefs();
    update::set_auto_check(auto_check);
    update::set_auto_install(auto_install);
    if !update::enabled() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("update-schedule".into())
        .spawn(move || {
            loop {
                let failed = update::auto_check() && check_blocking(&ah);
                let delay = update::next_delay(
                    update::interval(),
                    failed,
                    update::jitter(),
                    !update::interval_overridden(),
                );
                tracing::debug!(secs = delay.as_secs(), "next update check scheduled");
                std::thread::sleep(delay);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("update schedule thread failed to start: {e}");
    }
}

/// "Check Now", and switching automatic checks back on: one check, off-thread.
pub(crate) fn check_now(s: &mut State) {
    if !update::enabled() {
        return;
    }
    if let Some(ah) = s.app_handle.clone() {
        std::thread::spawn(move || {
            check_blocking(&ah);
        });
    }
}

/// One check, start to finish, on the calling (background) thread. Returns
/// whether it failed (the scheduler then retries sooner).
fn check_blocking(ah: &AppHandle<State>) -> bool {
    if !update::begin_check() {
        return false; // another check is already running
    }
    ah.update(|s: &mut State, _tx: &mut Tx| {
        // Don't paint over a download in progress or one that's ready.
        if matches!(
            s.update.status,
            UpdateStatus::Idle
                | UpdateStatus::UpToDate
                | UpdateStatus::Failed(_)
                | UpdateStatus::Available(_)
        ) {
            s.update.status = UpdateStatus::Checking;
        }
    });
    let result = update::check();
    update::end_check();
    match result {
        Ok(Some(found)) => {
            tracing::info!(version = %found.version, "update available");
            let f = found.clone();
            ah.update(move |s: &mut State, _tx: &mut Tx| on_found(s, f));
            if update::install_immediately() {
                install_now_with(ah.clone());
            } else if update::auto_install()
                && update::staged_version().as_deref() != Some(found.version.as_str())
            {
                download_for_quit(ah.clone(), found);
            }
            false
        }
        Ok(None) => {
            tracing::info!("no update available");
            ah.update(|s: &mut State, _tx: &mut Tx| {
                s.update.last_checked = Some(Instant::now());
                if s.update.status == UpdateStatus::Checking {
                    s.update.status = UpdateStatus::UpToDate;
                }
            });
            false
        }
        Err(e) => {
            tracing::info!("update check failed: {e}");
            let msg = e.to_string();
            ah.update(move |s: &mut State, _tx: &mut Tx| {
                s.update.last_checked = Some(Instant::now());
                if s.update.status == UpdateStatus::Checking {
                    s.update.status = UpdateStatus::Failed(msg);
                }
            });
            true
        }
    }
}

/// Fold a found release into the UI: offer it (unless that very version is
/// already downloading / ready / restarting) and announce it once per session.
pub(crate) fn on_found(s: &mut State, found: Found) {
    s.update.last_checked = Some(Instant::now());
    let busy_with_it = match &s.update.status {
        UpdateStatus::Downloading(f) | UpdateStatus::Ready(f) => f.version == found.version,
        UpdateStatus::Restarting(_) => true,
        _ => false,
    };
    if s.update.notified.as_deref() != Some(found.version.as_str()) {
        notify::update_available(&found.version);
        s.update.notified = Some(found.version.clone());
    }
    if !busy_with_it {
        s.update.status = UpdateStatus::Available(found);
    }
}

/// "Install & Restart": quit into the update now. A download that's already
/// ready (installed-on-quit) is reused; otherwise download + verify first.
pub(crate) fn install_now(s: &mut State) {
    if let Some(ah) = s.app_handle.clone() {
        install_now_with(ah);
    }
}

fn install_now_with(ah: AppHandle<State>) {
    if update::stage_relaunch() {
        let version = update::staged_version().unwrap_or_default();
        tracing::info!(version, "update already verified; quitting to install it");
        ah.update(move |s: &mut State, _tx: &mut Tx| {
            s.update.status = UpdateStatus::Restarting(version);
        });
        ah.quit();
        return;
    }
    ah.update(|s: &mut State, _tx: &mut Tx| {
        if let Some(f) = s.update.status.found().cloned() {
            s.update.status = UpdateStatus::Downloading(f);
        }
    });
    std::thread::spawn(move || match update::download(true) {
        Ok(Some(version)) => {
            tracing::info!(
                version,
                "update downloaded and verified; quitting to install it"
            );
            ah.update(move |s: &mut State, _tx: &mut Tx| {
                s.update.status = UpdateStatus::Restarting(version);
            });
            ah.quit();
        }
        Ok(None) => {
            tracing::info!("no update to install");
            ah.update(|s: &mut State, _tx: &mut Tx| s.update.status = UpdateStatus::UpToDate);
        }
        Err(e) => {
            tracing::error!("update download failed: {e}");
            let msg = e.to_string();
            ah.update(move |s: &mut State, _tx: &mut Tx| {
                s.update.status = UpdateStatus::Failed(msg);
            });
        }
    });
}

/// "Install updates automatically": fetch + verify in the background and stage
/// it to install when the user next quits — never mid-session.
fn download_for_quit(ah: AppHandle<State>, found: Found) {
    let f = found.clone();
    ah.update(move |s: &mut State, _tx: &mut Tx| s.update.status = UpdateStatus::Downloading(f));
    std::thread::spawn(move || match update::download(false) {
        Ok(Some(version)) => {
            tracing::info!(
                version,
                "update downloaded and verified; it installs when you quit"
            );
            let mut f = found;
            f.version = version;
            ah.update(move |s: &mut State, _tx: &mut Tx| {
                // The user may have hit "Install & Restart" or switched this off
                // meanwhile; only a still-downloading state becomes Ready.
                if matches!(s.update.status, UpdateStatus::Downloading(_)) && s.update.auto_install
                {
                    s.update.status = UpdateStatus::Ready(f);
                } else if !s.update.auto_install {
                    update::discard_staged();
                }
            });
        }
        Ok(None) => {
            ah.update(|s: &mut State, _tx: &mut Tx| s.update.status = UpdateStatus::UpToDate);
        }
        Err(e) => {
            tracing::error!("update download failed: {e}");
            let msg = e.to_string();
            ah.update(move |s: &mut State, _tx: &mut Tx| {
                s.update.status = UpdateStatus::Failed(msg);
            });
        }
    });
}

/// Settings switch: "Check for updates automatically". Turning it on checks at
/// once (the user just expressed interest); off stops the schedule's checks
/// ("Check Now" still works).
pub(crate) fn set_auto_check(s: &mut State, on: bool) {
    s.update.auto_check = on;
    config::save_update_auto_check(on);
    update::set_auto_check(on);
    if on {
        check_now(s);
    }
}

/// Settings switch: "Install updates automatically". On with an update on offer
/// starts its background download; off drops a downloaded one so quitting no
/// longer installs it (it stays offered for "Install & Restart").
pub(crate) fn set_auto_install(s: &mut State, on: bool) {
    s.update.auto_install = on;
    config::save_update_auto_install(on);
    update::set_auto_install(on);
    match (&s.update.status, on) {
        (UpdateStatus::Available(f), true) => {
            if let Some(ah) = s.app_handle.clone() {
                download_for_quit(ah, f.clone());
            }
        }
        (UpdateStatus::Ready(f) | UpdateStatus::Downloading(f), false) => {
            update::discard_staged();
            s.update.status = UpdateStatus::Available(f.clone());
        }
        _ => {}
    }
}
