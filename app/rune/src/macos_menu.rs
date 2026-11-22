//! macOS native menus: a **Controls** menu in the menu bar (Play/Pause, Next ⌘→,
//! Previous ⌘←, like Apple Music) and the same transport in the **Dock menu**
//! (right-click / long-press the Dock icon). Both are Rune native-menu items
//! (`MenuItem` → `NSMenu`), so no AppKit code lives here.
//!
//! Every item's behaviour is the pure [`command_for`] mapping, unit-tested on all
//! platforms; [`items`] is only wired into the app on macOS (Rune returns `Unsupported`
//! for native menus elsewhere — Windows/Linux keep the tray for the same controls).
#![forbid(unsafe_code)]

use rune::{MenuItem, Shortcut, Tx};
use soul_player_host::Command;

use crate::State;

/// Menu-bar menu name (Apple Music uses the same).
pub(crate) const CONTROLS_MENU: &str = "Controls";

// Stable ids (Rune requires unique ids across all custom items).
pub(crate) const PLAY_PAUSE: &str = "controls-play-pause";
pub(crate) const NEXT: &str = "controls-next";
pub(crate) const PREV: &str = "controls-prev";
pub(crate) const DOCK_PLAY_PAUSE: &str = "dock-play-pause";
pub(crate) const DOCK_NEXT: &str = "dock-next";
pub(crate) const DOCK_PREV: &str = "dock-prev";

/// The player command a native menu item sends (`None` for an unknown id).
pub(crate) fn command_for(id: &str) -> Option<Command> {
    match id {
        PLAY_PAUSE | DOCK_PLAY_PAUSE => Some(Command::TogglePlay),
        NEXT | DOCK_NEXT => Some(Command::Next),
        PREV | DOCK_PREV => Some(Command::Prev),
        _ => None,
    }
}

fn send(s: &mut State, id: &str) {
    if let Some(cmd) = command_for(id) {
        s.host.send(cmd);
    }
}

// `MenuItem` actions are plain `fn` pointers, so one tiny fn per id.
fn play_pause(s: &mut State, _: &mut Tx) {
    send(s, PLAY_PAUSE);
}
fn next(s: &mut State, _: &mut Tx) {
    send(s, NEXT);
}
fn prev(s: &mut State, _: &mut Tx) {
    send(s, PREV);
}

fn shortcut(spec: &str) -> Option<Shortcut> {
    spec.parse().ok()
}

/// The Controls menu + Dock menu items.
pub(crate) fn items() -> Vec<MenuItem<State>> {
    let play_pause_title = t!("tray.playPause").to_string();
    let next_title = t!("tray.next").to_string();
    let prev_title = t!("tray.prev").to_string();

    let mut controls = vec![
        MenuItem::new(play_pause_title.clone(), play_pause)
            .id(PLAY_PAUSE)
            .menu(CONTROLS_MENU),
        MenuItem::new(next_title.clone(), next)
            .id(NEXT)
            .menu(CONTROLS_MENU),
        MenuItem::new(prev_title.clone(), prev)
            .id(PREV)
            .menu(CONTROLS_MENU),
    ];
    // ⌘→ / ⌘← like Apple Music — `keys_first` so a focused text field (search, rename)
    // still gets its own ⌘→/⌘← (end/start of line) before the menu takes the chord.
    for (item, spec) in controls.iter_mut().skip(1).zip(["mod+right", "mod+left"]) {
        if let Some(sc) = shortcut(spec) {
            *item = item.clone().key_equivalent(sc).keys_first();
        }
    }
    let dock = vec![
        MenuItem::new(play_pause_title, play_pause)
            .id(DOCK_PLAY_PAUSE)
            .dock(),
        MenuItem::new(next_title, next).id(DOCK_NEXT).dock(),
        MenuItem::new(prev_title, prev).id(DOCK_PREV).dock(),
    ];
    controls.into_iter().chain(dock).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_id_maps_to_its_transport_command() {
        for id in [PLAY_PAUSE, DOCK_PLAY_PAUSE] {
            assert_eq!(command_for(id), Some(Command::TogglePlay), "{id}");
        }
        for id in [NEXT, DOCK_NEXT] {
            assert_eq!(command_for(id), Some(Command::Next), "{id}");
        }
        for id in [PREV, DOCK_PREV] {
            assert_eq!(command_for(id), Some(Command::Prev), "{id}");
        }
        assert_eq!(command_for("nope"), None);
    }

    #[test]
    fn menu_ids_are_unique_because_rune_rejects_duplicates() {
        let ids = [
            PLAY_PAUSE,
            NEXT,
            PREV,
            DOCK_PLAY_PAUSE,
            DOCK_NEXT,
            DOCK_PREV,
        ];
        let set: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(set.len(), ids.len());
    }

    #[test]
    fn the_arrow_key_equivalents_parse() {
        assert!(shortcut("mod+right").is_some());
        assert!(shortcut("mod+left").is_some());
    }

    #[test]
    fn items_cover_the_controls_menu_and_the_dock() {
        assert_eq!(items().len(), 6, "3 Controls items + 3 Dock items");
    }
}
