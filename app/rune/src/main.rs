//! Rune UI head for Soul Player — the one desktop UI. Wires
//! `soul_player_host::PlayerHandle` into a Rune `App`: builds the host, autoloads
//! a library from `SOUL_PLAYER_FOLDER`, and folds backend `Tick`/`Queue` events
//! into `State` via `AppHandle::update`. The view is a two-region shell: a
//! fixed-width `views::sidebar` (Library/Settings nav, `views::queue`,
//! `views::player` panel) on the left, and the routed main area — the
//! `views::library` track list or `views::settings` — filling the rest.
#![forbid(unsafe_code)]

#[macro_use]
extern crate rust_i18n;

// Compile-time-embed the translations in `locales/app.yml`; the crate-wide
// `t!("key")` macro then resolves against the current locale (set per the saved
// pref / OS language in `apply_locale`), falling back to English.
i18n!("locales", fallback = "en");

use std::path::PathBuf;

use rune::{App, AppSpec, Cx, El, Key, KeyEvent, Tx, Waker, col, row};
use soul_player_host::{
    BrowseAxis, Command, Event, PlayState, PlayerHandle, RepeatMode, TrackView, WatchUpdate,
};

mod config;
mod theme;
mod update;
mod views;

use views::player::ArtCache;

/// The UI locales shipped besides the English fallback (see `locales/app.yml`).
pub(crate) const LOCALES: &[&str] = &["en", "de", "es", "zh", "cs"];

/// Resolve a locale preference to a concrete locale code. A supported pref
/// ("de", "zh", …) is used as-is; "system" — or anything unsupported — matches
/// the OS language via `sys-locale`, falling back to English.
fn resolve_locale(pref: &str) -> &'static str {
    if let Some(&code) = LOCALES.iter().find(|&&c| c == pref) {
        return code;
    }
    let sys = sys_locale::get_locale().unwrap_or_default();
    let lang = sys.split(['-', '_']).next().unwrap_or("en").to_lowercase();
    LOCALES
        .iter()
        .copied()
        .find(|&c| c == lang.as_str())
        .unwrap_or("en")
}

/// Point the global `rust_i18n` locale at `pref`'s resolved code, so the next
/// render's `t!` calls translate against it.
fn apply_locale(pref: &str) {
    rust_i18n::set_locale(resolve_locale(pref));
}

/// Switch the UI language (Settings → Language): remember the pref, apply it, and
/// persist. The next frame re-renders in the new locale.
pub(crate) fn set_locale_pref(s: &mut State, pref: &str) {
    s.locale_pref = pref.to_string();
    apply_locale(pref);
    config::save_locale(pref);
}

/// Which page the main area shows. The sidebar switches it; `view` dispatches on it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Route {
    Library,
    Settings,
}

/// Light/dark preference: follow the OS, or force one. Persisted; the Appearance
/// settings section sets it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ThemePref {
    System,
    Light,
    Dark,
}

impl ThemePref {
    fn from_config(s: &str) -> Self {
        match s {
            "light" => ThemePref::Light,
            "dark" => ThemePref::Dark,
            _ => ThemePref::System,
        }
    }
    fn as_config(self) -> &'static str {
        match self {
            ThemePref::System => "system",
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
        }
    }
}

struct State {
    /// Read by the view's command handlers (click-to-play, transport).
    host: PlayerHandle,
    library: Vec<TrackView>,
    now_playing_id: Option<u32>,
    position_secs: f64,
    duration_secs: f64,
    playing: bool,
    volume: f32,
    shuffle: bool,
    repeat: RepeatMode,
    up_next: Vec<u32>,
    context_pos: u32,
    context_len: u32,
    /// Current page (Library vs Settings).
    route: Route,
    /// Light/dark preference (persisted); `System` follows the OS.
    theme_pref: ThemePref,
    /// Which baked axis the library list is ordered by.
    sort_axis: BrowseAxis,
    /// The watched-folder set (from the host), rendered by Settings → Sources;
    /// `unavailable` are configured roots currently unreachable (shown "offline").
    watched_folders: Vec<PathBuf>,
    unavailable: Vec<PathBuf>,
    /// Cover-art decode cache (`views::player::ArtCache`), wired up with the
    /// app's `Waker` in `setup_art` below.
    art: ArtCache,
    /// The version of a newer signed release, set by the launch-time update
    /// check (`update::available`); `Some` makes Settings surface "Install &
    /// Restart". `None` = up to date (or the check hasn't run / failed).
    update_available: Option<String>,
    /// Library filter query (case-insensitive substring over title/artist/album);
    /// empty = no filter. Rune has no text widget yet, so it's typed into via the
    /// key handler (`/` to start), not a focused input field.
    search: String,
    /// Whether typed characters currently build `search` (vs. driving the
    /// transport shortcuts). `/` or clicking the search bar turns it on; Esc/Enter
    /// turn it off.
    searching: bool,
    /// The resizable sidebar's width (clamped 240–480) and whether it's collapsed
    /// to a thin rail. Persisted via [`config::load_sidebar`]/[`config::save_sidebar`]
    /// and driven by the drag handle in `views::sidebar`.
    sidebar_width: f32,
    sidebar_collapsed: bool,
    /// UI language preference ("system"/"en"/"de"/…); drives the global
    /// `rust_i18n` locale via [`apply_locale`]. Changed by Settings → Language.
    locale_pref: String,
}

impl State {
    fn new(host: PlayerHandle) -> Self {
        let library = host.views();
        let watched_folders = host.watched_folders();
        // At startup the host has no fresh WatchUpdate, so derive the offline set
        // directly: a restored watched root that isn't a readable directory is
        // unavailable (and shows the badge). Live edits refresh it via apply_watch.
        let unavailable = watched_folders
            .iter()
            .filter(|p| !p.is_dir())
            .cloned()
            .collect();
        let (sidebar_width, sidebar_collapsed) = config::load_sidebar();
        let locale_pref = config::load_locale();
        apply_locale(&locale_pref);
        Self {
            host,
            library,
            now_playing_id: None,
            position_secs: 0.0,
            duration_secs: 0.0,
            playing: false,
            volume: 1.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            up_next: Vec::new(),
            context_pos: 0,
            context_len: 0,
            route: Route::Library,
            theme_pref: config::load_theme_pref()
                .map(|s| ThemePref::from_config(&s))
                .unwrap_or(ThemePref::System),
            sort_axis: BrowseAxis::ArtistAlbumTrack,
            watched_folders,
            unavailable,
            art: ArtCache::new(),
            update_available: None,
            search: String::new(),
            searching: false,
            sidebar_width,
            sidebar_collapsed,
            locale_pref,
        }
    }

    /// Resolve a library track by id. Shared by the now-playing panel
    /// (`now_playing_id`) and the sidebar queue (`up_next`) so both compose
    /// the same lookup rather than each re-scanning `library` inline.
    fn track(&self, id: u32) -> Option<&TrackView> {
        self.library.iter().find(|t| t.id == id)
    }

    /// Fold one backend event into the UI state (the UI's event sink).
    fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Tick(t) => {
                self.now_playing_id = t.now_playing_id;
                self.position_secs = t.position_secs;
                self.duration_secs = t.duration_secs.unwrap_or(0.0);
                self.playing = matches!(t.state, PlayState::Playing);
                self.volume = t.volume;
                self.shuffle = t.shuffle;
                self.repeat = t.repeat;
            }
            Event::Queue(q) => {
                self.up_next = q.up_next_ids.clone();
                self.context_pos = q.context_pos;
                self.context_len = q.context_len;
            }
        }
    }
}

/// Fold a `WatchUpdate` (from add/remove/set-folders) into UI state: refresh the
/// library + Sources list. Playback is deliberately left untouched — managing
/// what's watched is separate from what's playing (the host's own contract).
pub(crate) fn apply_watch(s: &mut State, up: WatchUpdate) {
    s.library = up.views;
    s.watched_folders = s.host.watched_folders();
    s.unavailable = up.unavailable;
}

/// Apply a `WatchUpdate` and persist the new watched set — used by the Sources
/// add/remove handlers. Kept separate from the pure `apply_watch` so unit tests
/// never touch the user's config on disk.
pub(crate) fn apply_and_save(s: &mut State, up: WatchUpdate) {
    apply_watch(s, up);
    config::save_sources(&s.watched_folders);
}

/// Resolve the effective dark/light mode from the user's preference.
pub(crate) fn is_dark(pref: ThemePref, system_dark: bool) -> bool {
    match pref {
        ThemePref::System => system_dark,
        ThemePref::Light => false,
        ThemePref::Dark => true,
    }
}

/// Set + persist the theme preference (used by the Appearance settings buttons).
pub(crate) fn set_theme_pref(s: &mut State, pref: ThemePref) {
    s.theme_pref = pref;
    config::save_theme_pref(pref.as_config());
}

/// Switch the library's sort order — re-fetches the current library's views in
/// the chosen baked axis. Track ids are stable across axes (only the display +
/// play-context order changes), so playback is undisturbed.
pub(crate) fn set_sort(s: &mut State, axis: BrowseAxis) {
    s.sort_axis = axis;
    s.library = s.host.views_by(axis);
}

fn view(s: &State, cx: &mut Cx<State>) -> El {
    let p = theme::palette(is_dark(s.theme_pref, cx.system_dark()));
    let scale = cx.env().scale;
    // The old app's shell: a fixed-width left sidebar (nav label, queue,
    // player panel) plus the main area's track list filling the rest.
    row()
        .w_full()
        .h_full()
        .fill(p.bg)
        .child(if s.sidebar_collapsed {
            views::sidebar::collapsed_strip(&p)
        } else {
            views::sidebar::sidebar(s, &p, cx, scale)
        })
        .child(views::sidebar::resize_handle())
        .child(col().grow(1.0).min_h(0.0).child(match s.route {
            Route::Library => views::library::track_list(s, &p).grow(1.0),
            Route::Settings => views::settings::settings_page(s, &p).grow(1.0),
        }))
}

/// `AppSpec::setup`: wires the app's `Waker` into the cover-art cache
/// (cheat-sheet §5b/§6a) so decoded covers wake a redraw instead of needing
/// a poll loop.
fn setup_art(s: &mut State, waker: Waker) {
    s.art.wire(waker);
}

/// `AppSpec::on_key`: transport and volume shortcuts, so the head is
/// drivable from the keyboard alone (Space, arrows, `s`/`r`). Only
/// key-*down* events ever reach `on_key` in the first place — Rune's
/// `Runtime::key`/physical-input dispatch never forwards a release as a
/// logical `KeyEvent` (`crates/rune/src/runtime/input/keys.rs`'s
/// `queue_key`/`physical_then`: a release with no text/IME fallback is
/// handled as a `PhysicalInput` observation, not queued as a `KeyEvent`) —
/// and `Tx::key_repeat()` flags the OS's auto-repeat of a held key, so a
/// held arrow doesn't re-fire every frame. Letters with no modifier arrive
/// as `Key::Text` (`Key::Command(c)` is reserved for Cmd/Ctrl chords), so
/// `s`/`r` are matched there.
fn handle_key(s: &mut State, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if tx.key_repeat() {
        return false;
    }
    // Search-input mode: typed characters build the library filter instead of
    // driving the transport. `/` enters it; Esc clears + exits, Enter just exits
    // (keeping the filter), Backspace edits.
    if s.searching {
        match &ev.key {
            Key::Escape => {
                s.search.clear();
                s.searching = false;
            }
            Key::Enter => s.searching = false,
            Key::Backspace => {
                s.search.pop();
            }
            Key::Text(t) => s.search.push_str(t),
            _ => return false,
        }
        return true;
    }
    match &ev.key {
        Key::Text(t) if t == "/" && matches!(s.route, Route::Library) => s.searching = true,
        Key::Text(t) if t == " " => s.host.send(Command::TogglePlay),
        Key::Right => s.host.send(Command::Next),
        Key::Left => s.host.send(Command::Prev),
        Key::Up => s.host.send(Command::SetVolume((s.volume + 0.05).min(1.0))),
        Key::Down => s.host.send(Command::SetVolume((s.volume - 0.05).max(0.0))),
        Key::Text(t) if t.eq_ignore_ascii_case("s") => {
            s.host.send(Command::SetShuffle(!s.shuffle));
        }
        Key::Text(t) if t.eq_ignore_ascii_case("r") => {
            let next = match s.repeat {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            s.host.send(Command::SetRepeat(next));
        }
        _ => return false,
    }
    true
}

/// Snapshot scenes for headless e2e rendering (`--scene NAME`): each is a named
/// tweak to the initial state so `--snap` can capture a specific view. Inert in
/// normal use — only Rune's tooling `--scene` flag ever runs them.
fn scene_settings(s: &mut State) {
    s.route = Route::Settings;
}
fn scene_search(s: &mut State) {
    s.search = "aria".to_string();
}
fn scene_no_match(s: &mut State) {
    s.search = "zzz-no-such-track".to_string();
}

fn main() -> anyhow::Result<()> {
    // Single-instance (release builds only — dev can run several for comparison):
    // a second launch focuses the running window (and forwards its links to
    // `on_open_url`) instead of opening a duplicate that would fight over the
    // audio device. Returns false when another instance handled this launch →
    // exit without opening a window. macOS/other platforms: always true.
    if !cfg!(debug_assertions) && !rune::single_instance("co.datadir.soulplayer") {
        return Ok(());
    }

    let host = PlayerHandle::new();
    // The dev/e2e harness loads a single folder from the env; otherwise restore
    // the persisted watched-folder set so Sources survive a restart.
    if std::env::var_os("SOUL_PLAYER_FOLDER").is_some() {
        host.autoload_from_env()?;
    } else {
        let saved = config::load_sources();
        if !saved.is_empty() {
            let _ = host.set_folders(&saved);
        }
    }

    let init_host = host.clone();
    let spec = AppSpec::<State> {
        setup: Some(setup_art),
        on_key: Some(handle_key),
        scenes: &[
            ("settings", scene_settings),
            ("search", scene_search),
            ("no-match", scene_no_match),
        ],
        ..AppSpec::new(
            "Soul Player (Rune)",
            (1100.0, 720.0),
            || unreachable!("init overridden"),
            view,
        )
    };
    let app = App::new(spec)
        // System-wide transport hotkeys (fire even when the window is unfocused).
        // Dedicated hardware media keys + OS "Now Playing" controls are a separate
        // integration (souvlaki) — these are user-reachable chords in the meantime.
        .global_shortcut("ctrl+alt+space", |s: &mut State, _: &mut Tx| {
            s.host.send(Command::TogglePlay)
        })
        .global_shortcut("ctrl+alt+right", |s: &mut State, _: &mut Tx| {
            s.host.send(Command::Next)
        })
        .global_shortcut("ctrl+alt+left", |s: &mut State, _: &mut Tx| {
            s.host.send(Command::Prev)
        })
        .init(move || State::new(init_host.clone()));
    let ah = app.handle();
    // Launch-time update check (release builds only): off-thread so it never
    // blocks startup, and it only *flags* an available update — Settings offers
    // the actual "Install & Restart" so we never yank a playing track away.
    if update::enabled() {
        let uh = app.handle();
        std::thread::spawn(move || {
            if let Some(version) = update::available() {
                uh.update(move |s: &mut State, _tx: &mut Tx| {
                    s.update_available = Some(version);
                });
            }
        });
    }
    host.subscribe(move |ev: Event| {
        ah.update(move |s: &mut State, tx: &mut Tx| {
            s.apply(&ev);
            tx.instant(); // don't animate fast-moving numeric fields
        });
    });
    app.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use rune::{Env, Mods, Runtime};

    use super::*;

    fn testdata_dir() -> PathBuf {
        // app/rune -> repo-root/testdata
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
    }

    /// Cold launch, no library: the empty-state's Open Folder button
    /// (`views::library::track_list`) is the only entry point, so it must be
    /// findable headlessly — `Runtime::new` + `rt.frame` + `rt.find`, the
    /// Rust-level harness `examples/hello`'s own test module uses
    /// (cheat-sheet §8). This is the `app/rune` head's whole
    /// headless-testable claim, proven with no window and no device.
    #[test]
    fn empty_library_points_to_sources() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            State::new(PlayerHandle::offline()),
            view,
            (1100.0, 720.0),
            env,
        );
        rt.frame(0.0);
        assert!(
            rt.find("go-to-sources").is_some(),
            "an empty library routes the user to Sources"
        );
        assert!(
            rt.find("open-folder").is_none(),
            "the single-folder Open Folder button is gone"
        );
    }

    #[test]
    fn new_state_starts_on_library_with_host_watched_folders() {
        let s = State::new(PlayerHandle::offline());
        assert!(matches!(s.route, Route::Library));
        assert!(s.watched_folders.is_empty());
        assert!(s.unavailable.is_empty());
    }

    #[test]
    fn new_state_derives_offline_badge_for_unavailable_watched_roots() {
        let missing = testdata_dir().join("does-not-exist-at-launch");
        let host = PlayerHandle::offline();
        host.set_folders(std::slice::from_ref(&missing)).unwrap();
        let s = State::new(host);
        assert_eq!(s.watched_folders, vec![missing.clone()]);
        assert_eq!(
            s.unavailable,
            vec![missing],
            "a restored, currently-unavailable root is offline at launch"
        );
    }

    #[test]
    fn apply_watch_folds_a_watchupdate_into_state() {
        let host = PlayerHandle::offline();
        let mut s = State::new(host);
        assert!(s.watched_folders.is_empty());
        // add_folder scans the committed testdata (real audio) and returns the
        // WatchUpdate the UI folds in.
        let up = s.host.add_folder(&testdata_dir()).expect("add_folder");
        apply_watch(&mut s, up);
        assert_eq!(s.watched_folders, vec![testdata_dir()]);
        assert!(!s.library.is_empty(), "testdata scanned into the library");
        assert!(s.unavailable.is_empty());
    }

    /// A `State` routed to Settings with a given watched set + unavailable list.
    fn settings_state(folders: &[PathBuf], unavailable: &[PathBuf]) -> State {
        let host = PlayerHandle::offline();
        if !folders.is_empty() {
            let _ = host.set_folders(folders);
        }
        let mut s = State::new(host);
        s.route = Route::Settings;
        s.unavailable = unavailable.to_vec();
        s
    }

    #[test]
    fn settings_route_shows_sources_section_and_add_button() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("sources").is_some(),
            "Settings shows a Sources section"
        );
        assert!(rt.find("add-source").is_some(), "Sources offers Add Source");
        assert!(rt.find("rescan-sources").is_some(), "Sources offers Rescan");
    }

    #[test]
    fn sidebar_resize_handle_present_and_collapses_to_a_rail() {
        // Expanded: the full sidebar and its always-present resize handle render,
        // and there is no collapsed rail.
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false; // explicit — don't depend on persisted config
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(rt.find("sidebar").is_some(), "the full sidebar renders");
        assert!(
            rt.find("sidebar-resize").is_some(),
            "the resize handle is present"
        );
        assert!(
            rt.find("sidebar-collapsed").is_none(),
            "no collapsed rail while expanded"
        );

        // Collapsed: the rail replaces the full sidebar; the handle stays so a
        // drag (or click) can re-open it.
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = true;
        let mut rt = Runtime::new(
            s,
            view,
            (1100.0, 720.0),
            Env {
                system_dark: true,
                scale: 2.0,
            },
        );
        rt.frame(0.0);
        assert!(
            rt.find("sidebar-collapsed").is_some(),
            "collapsing shows the thin rail"
        );
        assert!(
            rt.find("sidebar").is_none(),
            "the full sidebar is gone when collapsed"
        );
        assert!(
            rt.find("sidebar-resize").is_some(),
            "the handle remains, to re-open"
        );
    }

    #[test]
    fn sources_list_renders_a_watched_folder_with_remove() {
        let dir = testdata_dir();
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            settings_state(std::slice::from_ref(&dir), &[]),
            view,
            (1100.0, 720.0),
            env,
        );
        rt.frame(0.0);
        assert!(
            rt.find(&format!("source-{name}")).is_some(),
            "the watched folder renders"
        );
        assert!(
            rt.find(&format!("remove-{name}")).is_some(),
            "with a remove control"
        );
    }

    #[test]
    fn unavailable_folder_shows_offline_badge() {
        let missing = testdata_dir().join("does-not-exist");
        let name = missing.file_name().unwrap().to_string_lossy().to_string();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            settings_state(
                std::slice::from_ref(&missing),
                std::slice::from_ref(&missing),
            ),
            view,
            (1100.0, 720.0),
            env,
        );
        rt.frame(0.0);
        assert!(
            rt.find(&format!("offline-{name}")).is_some(),
            "an unavailable source shows the offline badge"
        );
    }

    #[test]
    fn sidebar_has_library_and_settings_nav() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            State::new(PlayerHandle::offline()),
            view,
            (1100.0, 720.0),
            env,
        );
        rt.frame(0.0);
        assert!(rt.find("nav-library").is_some());
        assert!(rt.find("nav-settings").is_some());
    }

    #[test]
    fn is_dark_respects_the_theme_preference() {
        assert!(is_dark(ThemePref::Dark, false));
        assert!(!is_dark(ThemePref::Light, true));
        assert!(is_dark(ThemePref::System, true));
        assert!(!is_dark(ThemePref::System, false));
    }

    #[test]
    fn settings_shows_appearance_theme_options() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("appearance").is_some(),
            "Settings has an Appearance section"
        );
        assert!(rt.find("theme-system").is_some());
        assert!(rt.find("theme-light").is_some());
        assert!(rt.find("theme-dark").is_some());
    }

    #[test]
    fn settings_shows_updates_section_without_install_by_default() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("updates").is_some(),
            "Settings has an Updates section"
        );
        assert!(
            rt.find("install-update").is_none(),
            "no install button until the launch check flags an update"
        );
    }

    #[test]
    fn settings_offers_install_when_update_available() {
        let mut s = settings_state(&[], &[]);
        s.update_available = Some("9.9.9".to_string());
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("install-update").is_some(),
            "a flagged update offers Install & Restart"
        );
    }

    #[test]
    fn library_shows_the_sort_bar_when_populated() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(State::new(host), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("sort-bar").is_some(),
            "populated library shows a sort bar"
        );
        for key in ["sort-artist", "sort-album", "sort-title", "sort-folder"] {
            assert!(rt.find(key).is_some(), "sort option {key} present");
        }
    }

    /// A populated library shows the search bar, and a matching query keeps the
    /// matching track visible.
    #[test]
    fn search_keeps_matching_tracks_and_shows_the_bar() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        assert!(!s.library.is_empty());
        let first = s.library[0].clone();
        let needle = first
            .title
            .clone()
            .or_else(|| first.artist.clone())
            .expect("first track has a title or artist to search for");
        s.search = needle;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("search-bar").is_some(), "the search bar renders");
        assert!(
            rt.find(&format!("track-{}", first.id)).is_some(),
            "the matching track stays visible"
        );
    }

    /// A query matching nothing filters every row out and shows the empty hint.
    #[test]
    fn search_with_no_matches_hides_rows() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        let first_id = s.library[0].id;
        s.search = "zzz-no-such-track-zzz".to_string();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("no-matches").is_some(),
            "a non-matching query shows the empty message"
        );
        assert!(
            rt.find(&format!("track-{first_id}")).is_none(),
            "non-matching rows are filtered out"
        );
    }

    /// Each track row offers an add-to-queue button (`Command::Enqueue`).
    #[test]
    fn track_rows_offer_add_to_queue() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let s = State::new(host);
        let first_id = s.library[0].id;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find(&format!("enqueue-{first_id}")).is_some(),
            "a track row offers an add-to-queue button"
        );
    }

    /// `/` on the Library route enters search mode; typing builds the query,
    /// Backspace edits it, Esc clears + exits.
    #[test]
    fn slash_enters_search_and_typing_builds_the_query() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(
            State::new(PlayerHandle::offline()),
            view,
            (1100.0, 720.0),
            env,
        );
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        let press = |rt: &mut Runtime<State>, key: Key| {
            rt.key(KeyEvent {
                key,
                mods: Mods::default(),
            });
            rt.frame(0.0); // keys dispatch on the next frame (Rune's own on_key tests)
        };
        press(&mut rt, Key::Text("/".into()));
        assert!(rt.state.searching, "/ enters search mode");
        press(&mut rt, Key::Text("a".into()));
        press(&mut rt, Key::Text("b".into()));
        assert_eq!(rt.state.search, "ab", "typed characters build the query");
        press(&mut rt, Key::Backspace);
        assert_eq!(rt.state.search, "a", "Backspace edits the query");
        press(&mut rt, Key::Escape);
        assert!(
            !rt.state.searching && rt.state.search.is_empty(),
            "Esc clears the query and exits search mode"
        );
    }

    /// End to end: `AppSpec::on_key`'s Space shortcut (set on the `Runtime`
    /// directly here — `crates/rune/src/tooling.rs` copies
    /// `AppSpec::on_key` into exactly this field) reaches the real backend
    /// via `Command::TogglePlay`, and the resulting `Tick` flips
    /// `State::playing` — not merely that `handle_key` returns `true`.
    /// Since the actor's events arrive off a real pump thread
    /// (`soul_player_host::PlayerHandle::subscribe`), this uses the same
    /// bounded `subscribe` + `recv_timeout` pattern as that crate's own
    /// `subscribe_receives_now_playing_tick` test.
    // Ignored in CI: this drives the real actor over NullSink, whose
    // instant-drain timing is environment-dependent. On a slow CI runner the
    // Space press can land after the 38-track context has already drained, so
    // the wait for a paused Tick never resolves and the loop spins forever (it
    // hung CI for hours). Runs locally with `cargo test -- --ignored`. A
    // deterministic re-enable would pin playback with `Repeat::One` and drain
    // the event backlog before pressing Space.
    #[test]
    #[ignore = "flaky actor-drain timing; can hang on slow CI — run with --ignored"]
    fn space_key_toggles_playback_through_the_real_backend() {
        let host = PlayerHandle::offline();
        let (tx, rx) = mpsc::channel::<Event>();
        host.subscribe(move |ev| {
            let _ = tx.send(ev);
        });
        host.open_folder(&testdata_dir()).expect("scan testdata/");

        let mut state = State::new(host);
        let deadline = Duration::from_secs(5);
        while let Ok(ev) = rx.recv_timeout(deadline) {
            state.apply(&ev);
            if state.playing {
                break;
            }
        }
        assert!(state.playing, "open_folder should autoplay track 0");

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(state, view, (1100.0, 720.0), env);
        rt.on_key = Some(handle_key);
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Text(" ".into()),
            mods: Mods::default(),
        });
        rt.frame(1.0 / 60.0);

        while let Ok(ev) = rx.recv_timeout(deadline) {
            rt.state.apply(&ev);
            if !rt.state.playing {
                break;
            }
        }
        assert!(!rt.state.playing, "Space should have paused playback");
    }
}
