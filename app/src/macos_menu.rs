//! The macOS menu bar and Dock menu (#66), as Rune native-menu items (`MenuItem` →
//! `NSMenu`), so no AppKit code lives here:
//!
//! - **Soul Player** (the app menu, after About): Check for Updates…, Settings… ⌘,
//!   (About, Services, Hide, Quit ⌘Q are AppKit's own).
//! - **Edit** / **Window**: Rune's standard items (Copy/Paste…, Minimize ⌘M, Close ⌘W).
//! - **View**: the sidebar's pages ⌘1–⌘6, Zoom In ⌘+, Zoom Out ⌘−, Actual Size ⌘0.
//! - **Controls**: Play/Pause, Next ⌘→, Previous ⌘←, volume ⌘↑ / ⌘↓, Mute ⌥⌘↓,
//!   Shuffle and Repeat with a checkmark on the current state (like Apple Music).
//! - **Help**: Report a Problem…, the website.
//! - **Dock menu**: Play/Pause, Next, Previous.
//!
//! Every item does what [`run`] says for its id, and that is plain state code,
//! unit-tested on every platform; [`items`] is only installed on macOS (Rune's
//! native menus are macOS-only; Windows/Linux keep their tray and in-window controls).
//! Menu names stay English internally (they identify the menu); what the user
//! reads comes from [`label`], so the standard items are translated too.
#![forbid(unsafe_code)]
// Only macOS installs the menus; elsewhere the items exist for the tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use rune::{Label, MenuItem, Shortcut, Tx};
use soul_player_host::{Command, RepeatMode};

use crate::{Route, SettingsTab, State};

pub(crate) const CONTROLS_MENU: &str = "Controls";
pub(crate) const VIEW_MENU: &str = "View";
pub(crate) const HELP_MENU: &str = "Help";

pub(crate) const CHECK_UPDATES: &str = "app-check-updates";
pub(crate) const SETTINGS: &str = "app-settings";
pub(crate) const VIEW_HOME: &str = "view-home";
pub(crate) const VIEW_TRACKS: &str = "view-tracks";
pub(crate) const VIEW_ALBUMS: &str = "view-albums";
pub(crate) const VIEW_ARTISTS: &str = "view-artists";
pub(crate) const VIEW_GENRES: &str = "view-genres";
pub(crate) const VIEW_PLAYLISTS: &str = "view-playlists";
pub(crate) const ZOOM_IN: &str = "view-zoom-in";
pub(crate) const ZOOM_OUT: &str = "view-zoom-out";
pub(crate) const ZOOM_RESET: &str = "view-zoom-reset";
pub(crate) const PLAY_PAUSE: &str = "controls-play-pause";
pub(crate) const NEXT: &str = "controls-next";
pub(crate) const PREV: &str = "controls-prev";
pub(crate) const VOLUME_UP: &str = "controls-volume-up";
pub(crate) const VOLUME_DOWN: &str = "controls-volume-down";
pub(crate) const MUTE: &str = "controls-mute";
pub(crate) const SHUFFLE: &str = "controls-shuffle";
pub(crate) const REPEAT_OFF: &str = "controls-repeat-off";
pub(crate) const REPEAT_ALL: &str = "controls-repeat-all";
pub(crate) const REPEAT_ONE: &str = "controls-repeat-one";
pub(crate) const REPORT_PROBLEM: &str = "help-report";
pub(crate) const WEBSITE: &str = "help-website";
pub(crate) const DOCK_PLAY_PAUSE: &str = "dock-play-pause";
pub(crate) const DOCK_NEXT: &str = "dock-next";
pub(crate) const DOCK_PREV: &str = "dock-prev";

const WEBSITE_URL: &str = "https://player.soulaudio.co";

/// The player state the menus show (checkmarks). Installed menus are rebuilt
/// when this, or the language, changes ([`sync`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Shown {
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub muted: bool,
}

impl Shown {
    pub(crate) fn of(s: &State) -> Self {
        Self {
            shuffle: s.shuffle,
            repeat: s.repeat,
            muted: s.muted,
        }
    }
}

/// Whether item `id` shows a checkmark: Mute when muted, Shuffle when on, and
/// the Repeat item of the current mode.
pub(crate) fn is_checked(shown: Shown, id: &str) -> bool {
    match id {
        MUTE => shown.muted,
        SHUFFLE => shown.shuffle,
        REPEAT_OFF => shown.repeat == RepeatMode::Off,
        REPEAT_ALL => shown.repeat == RepeatMode::All,
        REPEAT_ONE => shown.repeat == RepeatMode::One,
        _ => false,
    }
}

/// The page a View item opens.
fn route_for(id: &str) -> Option<Route> {
    Some(match id {
        VIEW_HOME => Route::Home,
        VIEW_TRACKS => Route::Library,
        VIEW_ALBUMS => Route::Albums,
        VIEW_ARTISTS => Route::Artists,
        VIEW_GENRES => Route::Genres,
        VIEW_PLAYLISTS => Route::Playlists,
        _ => return None,
    })
}

/// What a menu item does, by id. False for an unknown id.
pub(crate) fn run(s: &mut State, id: &str) -> bool {
    if let Some(route) = route_for(id) {
        crate::navigate_to(s, route, None, None, None, None);
        return true;
    }
    match id {
        SETTINGS => crate::navigate_to(s, Route::Settings, None, None, None, None),
        CHECK_UPDATES => {
            crate::navigate_to(s, Route::Settings, None, None, None, None);
            crate::set_settings_tab(s, SettingsTab::Updates);
            crate::updater::check_now(s);
        }
        ZOOM_IN => crate::set_zoom(s, crate::stepped_zoom(s.ui_zoom, 1)),
        ZOOM_OUT => crate::set_zoom(s, crate::stepped_zoom(s.ui_zoom, -1)),
        ZOOM_RESET => crate::set_zoom(s, 1.0),
        PLAY_PAUSE | DOCK_PLAY_PAUSE => s.host.send(Command::TogglePlay),
        NEXT | DOCK_NEXT => s.host.send(Command::Next),
        PREV | DOCK_PREV => s.host.send(Command::Prev),
        VOLUME_UP => crate::set_volume(s, s.volume + 0.05),
        VOLUME_DOWN => crate::set_volume(s, s.volume - 0.05),
        MUTE => crate::toggle_mute(s),
        SHUFFLE => s.host.send(Command::SetShuffle(!s.shuffle)),
        REPEAT_OFF => s.host.send(Command::SetRepeat(RepeatMode::Off)),
        REPEAT_ALL => s.host.send(Command::SetRepeat(RepeatMode::All)),
        REPEAT_ONE => s.host.send(Command::SetRepeat(RepeatMode::One)),
        REPORT_PROBLEM => {
            let device = s.output_device.clone();
            std::thread::spawn(move || crate::report::open_issue(device));
        }
        WEBSITE => {
            std::thread::spawn(|| {
                if let Err(e) = open::that(WEBSITE_URL) {
                    tracing::warn!("could not open {WEBSITE_URL}: {e}");
                }
            });
        }
        _ => return false,
    }
    true
}

// `MenuItem` actions are plain `fn` pointers, so one per id, each a call to `run`.
macro_rules! actions {
    ($($f:ident => $id:ident),* $(,)?) => {
        $(fn $f(s: &mut State, _: &mut Tx) { run(s, $id); })*
    };
}
actions! {
    check_updates => CHECK_UPDATES, settings => SETTINGS,
    view_home => VIEW_HOME, view_tracks => VIEW_TRACKS, view_albums => VIEW_ALBUMS,
    view_artists => VIEW_ARTISTS, view_genres => VIEW_GENRES, view_playlists => VIEW_PLAYLISTS,
    zoom_in => ZOOM_IN, zoom_out => ZOOM_OUT, zoom_reset => ZOOM_RESET,
    play_pause => PLAY_PAUSE, next => NEXT, prev => PREV,
    volume_up => VOLUME_UP, volume_down => VOLUME_DOWN, mute => MUTE,
    shuffle => SHUFFLE, repeat_off => REPEAT_OFF, repeat_all => REPEAT_ALL, repeat_one => REPEAT_ONE,
    report_problem => REPORT_PROBLEM, website => WEBSITE,
    dock_play_pause => DOCK_PLAY_PAUSE, dock_next => DOCK_NEXT, dock_prev => DOCK_PREV,
}

/// A menu item's action.
type Action = fn(&mut State, &mut Tx);

fn shortcut(spec: &str) -> Option<Shortcut> {
    spec.parse().ok()
}

/// An item with a key equivalent. `keys_first`: the key reaches a focused text
/// field and the app's own key handling first (⌘← in the search field, the
/// in-app ⌘= zoom), so a chord never acts twice; the menu takes it otherwise.
fn keyed(item: MenuItem<State>, spec: &str) -> MenuItem<State> {
    match shortcut(spec) {
        Some(sc) => item.key_equivalent(sc).keys_first(),
        None => item,
    }
}

fn tr(key: &str) -> String {
    t!(key).to_string()
}

/// Every custom item, in menu order, with `shown`'s checkmarks.
pub(crate) fn items(shown: Shown) -> Vec<MenuItem<State>> {
    let app = vec![
        MenuItem::new(tr("menu.checkUpdates"), check_updates).id(CHECK_UPDATES),
        keyed(
            MenuItem::new(tr("menu.settings"), settings).id(SETTINGS),
            "mod+,",
        ),
    ];

    let pages: [(&str, &str, Action, &str); 6] = [
        (VIEW_HOME, "nav.home", view_home, "mod+1"),
        (VIEW_TRACKS, "nav.tracks", view_tracks, "mod+2"),
        (VIEW_ALBUMS, "nav.albums", view_albums, "mod+3"),
        (VIEW_ARTISTS, "nav.artists", view_artists, "mod+4"),
        (VIEW_GENRES, "nav.genres", view_genres, "mod+5"),
        (VIEW_PLAYLISTS, "nav.playlists", view_playlists, "mod+6"),
    ];
    let mut view: Vec<_> = pages
        .into_iter()
        .map(|(id, key, f, sc)| keyed(MenuItem::new(tr(key), f).id(id).menu(VIEW_MENU), sc))
        .collect();
    view.push(MenuItem::separator().menu(VIEW_MENU));
    view.extend([
        keyed(
            MenuItem::new(tr("menu.zoomIn"), zoom_in)
                .id(ZOOM_IN)
                .menu(VIEW_MENU),
            "mod+=",
        ),
        keyed(
            MenuItem::new(tr("menu.zoomOut"), zoom_out)
                .id(ZOOM_OUT)
                .menu(VIEW_MENU),
            "mod+-",
        ),
        keyed(
            MenuItem::new(tr("menu.actualSize"), zoom_reset)
                .id(ZOOM_RESET)
                .menu(VIEW_MENU),
            "mod+0",
        ),
    ]);

    let controls = vec![
        MenuItem::new(tr("tray.playPause"), play_pause)
            .id(PLAY_PAUSE)
            .menu(CONTROLS_MENU),
        keyed(
            MenuItem::new(tr("tray.next"), next)
                .id(NEXT)
                .menu(CONTROLS_MENU),
            "mod+right",
        ),
        keyed(
            MenuItem::new(tr("tray.prev"), prev)
                .id(PREV)
                .menu(CONTROLS_MENU),
            "mod+left",
        ),
        MenuItem::separator().menu(CONTROLS_MENU),
        keyed(
            MenuItem::new(tr("menu.volumeUp"), volume_up)
                .id(VOLUME_UP)
                .menu(CONTROLS_MENU),
            "mod+up",
        ),
        keyed(
            MenuItem::new(tr("menu.volumeDown"), volume_down)
                .id(VOLUME_DOWN)
                .menu(CONTROLS_MENU),
            "mod+down",
        ),
        keyed(
            MenuItem::new(tr("player.mute"), mute)
                .id(MUTE)
                .menu(CONTROLS_MENU)
                .checked(is_checked(shown, MUTE)),
            "mod+alt+down",
        ),
        MenuItem::separator().menu(CONTROLS_MENU),
        MenuItem::new(tr("player.shuffle"), shuffle)
            .id(SHUFFLE)
            .menu(CONTROLS_MENU)
            .checked(is_checked(shown, SHUFFLE)),
        MenuItem::new(tr("menu.repeatOff"), repeat_off)
            .id(REPEAT_OFF)
            .menu(CONTROLS_MENU)
            .checked(is_checked(shown, REPEAT_OFF)),
        MenuItem::new(tr("player.repeatAll"), repeat_all)
            .id(REPEAT_ALL)
            .menu(CONTROLS_MENU)
            .checked(is_checked(shown, REPEAT_ALL)),
        MenuItem::new(tr("player.repeatOne"), repeat_one)
            .id(REPEAT_ONE)
            .menu(CONTROLS_MENU)
            .checked(is_checked(shown, REPEAT_ONE)),
    ];

    let help = vec![
        MenuItem::new(tr("menu.reportProblem"), report_problem)
            .id(REPORT_PROBLEM)
            .menu(HELP_MENU),
        MenuItem::new(tr("menu.website"), website)
            .id(WEBSITE)
            .menu(HELP_MENU),
    ];

    let dock = vec![
        MenuItem::new(tr("tray.playPause"), dock_play_pause)
            .id(DOCK_PLAY_PAUSE)
            .dock(),
        MenuItem::new(tr("tray.next"), dock_next)
            .id(DOCK_NEXT)
            .dock(),
        MenuItem::new(tr("tray.prev"), dock_prev)
            .id(DOCK_PREV)
            .dock(),
    ];

    [app, view, controls, help, dock].concat()
}

/// What the user reads for Rune's own labels: the menu names and the standard
/// Edit / Window / app-menu items, in the app's language. `None` keeps English.
pub(crate) fn label(l: Label<'_>) -> Option<String> {
    let key = match l {
        Label::Menu("Edit") => "menu.edit",
        Label::Menu("Window") => "menu.window",
        Label::Menu(VIEW_MENU) => "menu.view",
        Label::Menu(CONTROLS_MENU) => "menu.controls",
        Label::Menu(HELP_MENU) => "menu.help",
        Label::Undo => "menu.undo",
        Label::Redo => "menu.redo",
        Label::Cut => "menu.cut",
        Label::Copy => "menu.copy",
        Label::Paste => "menu.paste",
        Label::SelectAll => "menu.selectAll",
        Label::Minimize => "menu.minimize",
        Label::Close => "menu.close",
        Label::Services => "menu.services",
        Label::HideOthers => "menu.hideOthers",
        Label::ShowAll => "menu.showAll",
        Label::About(name) => return Some(t!("menu.about", name = name).to_string()),
        Label::Hide(name) => return Some(t!("menu.hide", name = name).to_string()),
        Label::Quit(name) => return Some(t!("menu.quit", name = name).to_string()),
        _ => return None,
    };
    Some(tr(key))
}

/// Re-install the menus when what they show changed: a checkmark (shuffle,
/// repeat, mute, from the player's tick) or the language. Cheap when nothing did.
pub(crate) fn sync(s: &mut State) {
    let now = (Shown::of(s), rust_i18n::locale().to_string());
    if s.menu_shown.as_ref() == Some(&now) {
        return;
    }
    let Some(handle) = s.app_handle.clone() else {
        return;
    };
    match handle.set_menu(items(now.0)) {
        Ok(()) => s.menu_shown = Some(now),
        // Before the event loop runs, or off macOS: the startup items stand.
        Err(rune::MenuError::NotRunning | rune::MenuError::Unsupported) => {}
        Err(e) => {
            tracing::warn!("menu bar update failed: {e}");
            s.menu_shown = Some(now); // don't retry every tick
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soul_player_host::PlayerHandle;

    const ALL: [&str; 26] = [
        CHECK_UPDATES,
        SETTINGS,
        VIEW_HOME,
        VIEW_TRACKS,
        VIEW_ALBUMS,
        VIEW_ARTISTS,
        VIEW_GENRES,
        VIEW_PLAYLISTS,
        ZOOM_IN,
        ZOOM_OUT,
        ZOOM_RESET,
        PLAY_PAUSE,
        NEXT,
        PREV,
        VOLUME_UP,
        VOLUME_DOWN,
        MUTE,
        SHUFFLE,
        REPEAT_OFF,
        REPEAT_ALL,
        REPEAT_ONE,
        REPORT_PROBLEM,
        WEBSITE,
        DOCK_PLAY_PAUSE,
        DOCK_NEXT,
        DOCK_PREV,
    ];

    fn recording() -> State {
        State::new(PlayerHandle::recording())
    }

    fn sent(id: &str, prepare: impl FnOnce(&mut State)) -> Vec<Command> {
        let mut s = recording();
        prepare(&mut s);
        assert!(run(&mut s, id), "{id} is handled");
        s.host.recorded_commands()
    }

    #[test]
    fn transport_items_send_their_commands() {
        for id in [PLAY_PAUSE, DOCK_PLAY_PAUSE] {
            assert_eq!(sent(id, |_| {}), vec![Command::TogglePlay], "{id}");
        }
        for id in [NEXT, DOCK_NEXT] {
            assert_eq!(sent(id, |_| {}), vec![Command::Next], "{id}");
        }
        for id in [PREV, DOCK_PREV] {
            assert_eq!(sent(id, |_| {}), vec![Command::Prev], "{id}");
        }
        assert_eq!(sent(MUTE, |_| {}), vec![Command::ToggleMute]);
    }

    #[test]
    fn shuffle_toggles_and_repeat_items_pick_their_mode() {
        assert_eq!(
            sent(SHUFFLE, |s| s.shuffle = false),
            vec![Command::SetShuffle(true)]
        );
        assert_eq!(
            sent(SHUFFLE, |s| s.shuffle = true),
            vec![Command::SetShuffle(false)]
        );
        assert_eq!(
            sent(REPEAT_OFF, |_| {}),
            vec![Command::SetRepeat(RepeatMode::Off)]
        );
        assert_eq!(
            sent(REPEAT_ALL, |_| {}),
            vec![Command::SetRepeat(RepeatMode::All)]
        );
        assert_eq!(
            sent(REPEAT_ONE, |_| {}),
            vec![Command::SetRepeat(RepeatMode::One)]
        );
    }

    #[test]
    fn volume_items_step_by_five_percent() {
        let up = sent(VOLUME_UP, |s| s.volume = 0.5);
        assert!(
            matches!(up.as_slice(), [Command::SetVolume(v)] if (v - 0.55).abs() < 1e-6),
            "{up:?}"
        );
        let down = sent(VOLUME_DOWN, |s| s.volume = 0.5);
        assert!(
            matches!(down.as_slice(), [Command::SetVolume(v)] if (v - 0.45).abs() < 1e-6),
            "{down:?}"
        );
    }

    #[test]
    fn view_items_open_their_pages() {
        let _cfg = crate::config::use_test_config_dir();
        for (id, route) in [
            (VIEW_HOME, Route::Home),
            (VIEW_TRACKS, Route::Library),
            (VIEW_ALBUMS, Route::Albums),
            (VIEW_ARTISTS, Route::Artists),
            (VIEW_GENRES, Route::Genres),
            (VIEW_PLAYLISTS, Route::Playlists),
        ] {
            let mut s = recording();
            s.route = Route::Settings;
            assert!(run(&mut s, id));
            assert!(s.route == route, "{id} opens its page");
        }
    }

    #[test]
    fn settings_and_check_for_updates_open_settings() {
        let _cfg = crate::config::use_test_config_dir();
        let mut s = recording();
        assert!(run(&mut s, SETTINGS));
        assert!(s.route == Route::Settings);
        let mut s = recording();
        s.settings_tab = SettingsTab::Appearance;
        assert!(run(&mut s, CHECK_UPDATES));
        assert!(s.route == Route::Settings);
        assert!(
            s.settings_tab == SettingsTab::Updates,
            "lands on Settings → Updates"
        );
    }

    #[test]
    fn zoom_items_step_and_reset() {
        let _cfg = crate::config::use_test_config_dir();
        let mut s = recording();
        s.ui_zoom = 1.0;
        run(&mut s, ZOOM_IN);
        run(&mut s, ZOOM_IN);
        assert!((s.ui_zoom - 1.2).abs() < 1e-4, "{}", s.ui_zoom);
        run(&mut s, ZOOM_OUT);
        assert!((s.ui_zoom - 1.1).abs() < 1e-4, "{}", s.ui_zoom);
        run(&mut s, ZOOM_RESET);
        assert!((s.ui_zoom - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn unknown_ids_do_nothing() {
        let mut s = recording();
        assert!(!run(&mut s, "nope"));
        assert!(s.host.recorded_commands().is_empty());
    }

    #[test]
    fn every_id_is_handled_unique_and_installed() {
        let mut seen = std::collections::HashSet::new();
        for id in ALL {
            assert!(seen.insert(id), "duplicate id {id} (Rune rejects them)");
        }
        // Every installable item is one of ALL (bar separators), and vice versa.
        let n = items(Shown::default()).len();
        assert_eq!(n, ALL.len() + 3, "26 items + 3 separators");
        // The help items open a browser; everything else is checked by running it.
        for id in ALL
            .iter()
            .filter(|id| ![REPORT_PROBLEM, WEBSITE, CHECK_UPDATES].contains(id))
        {
            let _cfg = crate::config::use_test_config_dir();
            assert!(run(&mut recording(), id), "{id} is handled");
        }
    }

    #[test]
    fn every_key_equivalent_parses() {
        for spec in [
            "mod+,",
            "mod+1",
            "mod+2",
            "mod+3",
            "mod+4",
            "mod+5",
            "mod+6",
            "mod+=",
            "mod+-",
            "mod+0",
            "mod+right",
            "mod+left",
            "mod+up",
            "mod+down",
            "mod+alt+down",
        ] {
            assert!(shortcut(spec).is_some(), "{spec} parses");
        }
    }

    #[test]
    fn standard_labels_are_translated_and_name_the_app() {
        let _l = crate::config::use_test_locale("de");
        assert_eq!(label(Label::Menu("Edit")).as_deref(), Some("Bearbeiten"));
        assert_eq!(
            label(Label::Menu(CONTROLS_MENU)).as_deref(),
            Some("Steuerung")
        );
        assert_eq!(
            label(Label::Quit("Soul Player")).as_deref(),
            Some("Soul Player beenden")
        );
        assert_eq!(
            label(Label::About("Soul Player")).as_deref(),
            Some("Über Soul Player")
        );
        assert_eq!(
            label(Label::LearnSpelling),
            None,
            "unmapped labels keep Rune's English"
        );
    }

    #[test]
    fn every_menu_string_exists_in_every_language() {
        for loc in ["en", "de", "es", "zh", "cs", "ja"] {
            let _l = crate::config::use_test_locale(loc);
            for key in [
                "menu.checkUpdates",
                "menu.settings",
                "menu.zoomIn",
                "menu.zoomOut",
                "menu.actualSize",
                "menu.volumeUp",
                "menu.volumeDown",
                "menu.repeatOff",
                "menu.reportProblem",
                "menu.website",
                "menu.edit",
                "menu.window",
                "menu.view",
                "menu.controls",
                "menu.help",
                "menu.undo",
                "menu.redo",
                "menu.cut",
                "menu.copy",
                "menu.paste",
                "menu.selectAll",
                "menu.minimize",
                "menu.close",
                "menu.services",
                "menu.hideOthers",
                "menu.showAll",
            ] {
                let v = t!(key).to_string();
                assert!(!v.is_empty() && v != key, "{loc}: {key} is missing");
            }
            for key in ["menu.about", "menu.hide", "menu.quit"] {
                let v = t!(key, name = "Soul Player").to_string();
                assert!(v.contains("Soul Player") && v != key, "{loc}: {key} = {v}");
            }
        }
    }

    #[test]
    fn checkmarks_follow_the_player() {
        let checked = |shown: Shown| -> Vec<&str> {
            ALL.into_iter().filter(|id| is_checked(shown, id)).collect()
        };
        assert_eq!(checked(Shown::default()), vec![REPEAT_OFF]);
        let all = checked(Shown {
            shuffle: true,
            repeat: RepeatMode::All,
            muted: true,
        });
        assert_eq!(all, vec![MUTE, SHUFFLE, REPEAT_ALL]);
        let one = checked(Shown {
            repeat: RepeatMode::One,
            ..Shown::default()
        });
        assert_eq!(one, vec![REPEAT_ONE]);
    }
}
