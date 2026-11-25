//! System-tray icon with quick playback controls (`rune::Tray`), so the player
//! can be driven while its window is hidden or backgrounded. Menu picks route to
//! player [`Command`]s or window actions; the primary click brings the window up.
#![forbid(unsafe_code)]
// On macOS this module is compiled for its tests only (no tray backend there).
#![cfg_attr(all(target_os = "macos", test), allow(dead_code))]

use rune::{AppHandle, Tray, TrayEvent, TrayIcon, TrayItem, TraySpec};
use soul_player_host::{Command, PlayerHandle};

use crate::State;

// Menu item ids (stable; returned in `TrayEvent::Menu`).
const PLAY_PAUSE: &str = "tray-play-pause";
const NEXT: &str = "tray-next";
const PREV: &str = "tray-prev";
const SHOW: &str = "tray-show";
const QUIT: &str = "tray-quit";

/// What a tray menu pick resolves to.
#[derive(Debug, PartialEq)]
pub(crate) enum TrayAction {
    Command(Command),
    /// Bring the window to the front.
    Show,
    /// Quit the app.
    Quit,
}

/// Map a tray menu id to an action (`None` for an unknown id).
pub(crate) fn menu_action(id: &str) -> Option<TrayAction> {
    match id {
        PLAY_PAUSE => Some(TrayAction::Command(Command::TogglePlay)),
        NEXT => Some(TrayAction::Command(Command::Next)),
        PREV => Some(TrayAction::Command(Command::Prev)),
        SHOW => Some(TrayAction::Show),
        QUIT => Some(TrayAction::Quit),
        _ => None,
    }
}

/// Decode the bundled app icon (PNG) to RGBA for the tray.
fn icon() -> Option<TrayIcon> {
    let img = image::load_from_memory(include_bytes!("../icons/icon.png"))
        .ok()?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Some(TrayIcon::rgba(w, h, img.into_raw()))
}

/// The tray menu: transport controls, then show / quit.
fn menu() -> Vec<TrayItem> {
    vec![
        TrayItem::action(PLAY_PAUSE, t!("tray.playPause").to_string()),
        TrayItem::action(NEXT, t!("tray.next").to_string()),
        TrayItem::action(PREV, t!("tray.prev").to_string()),
        TrayItem::Separator,
        TrayItem::action(SHOW, t!("tray.show").to_string()),
        TrayItem::Separator,
        TrayItem::action(QUIT, t!("tray.quit").to_string()),
    ]
}

/// Create the tray icon and wire its events. The returned [`Tray`] must stay alive
/// for the app's lifetime (dropping it removes the icon), so the caller leaks it.
/// `None` where the platform has no tray / creation failed.
pub(crate) fn create(host: PlayerHandle, ah: AppHandle<State>) -> Option<Tray> {
    let spec = TraySpec::new("soul-player", "Soul Player", icon()?)
        .tooltip("Soul Player")
        .menu(menu());
    match Tray::new(spec, move |event| handle(&host, &ah, event)) {
        Ok(tray) => {
            tracing::info!("system tray active");
            Some(tray)
        }
        Err(e) => {
            tracing::warn!("system tray unavailable: {e:?}");
            None
        }
    }
}

/// Route a tray event (runs on the tray thread) to a player command or window
/// action. `host`/`ah` both post across threads safely.
fn handle(host: &PlayerHandle, ah: &AppHandle<State>, event: TrayEvent) {
    match event {
        // Primary click: bring the window up (with the compositor token if given).
        TrayEvent::Activate { token } => match token {
            Some(t) => ah.activate(t),
            None => ah.open(),
        },
        TrayEvent::Menu(id) => match menu_action(&id) {
            Some(TrayAction::Command(cmd)) => host.send(cmd),
            Some(TrayAction::Show) => ah.open(),
            Some(TrayAction::Quit) => ah.quit(),
            None => {}
        },
        // `TrayEvent` is `#[non_exhaustive]`; ignore anything new.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_actions() {
        assert_eq!(
            menu_action(PLAY_PAUSE),
            Some(TrayAction::Command(Command::TogglePlay))
        );
        assert_eq!(menu_action(NEXT), Some(TrayAction::Command(Command::Next)));
        assert_eq!(menu_action(PREV), Some(TrayAction::Command(Command::Prev)));
        assert_eq!(menu_action(SHOW), Some(TrayAction::Show));
        assert_eq!(menu_action(QUIT), Some(TrayAction::Quit));
        assert_eq!(menu_action("nope"), None);
    }
}
