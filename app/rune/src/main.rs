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

use rune::{App, AppSpec, Cx, El, Key, KeyEvent, Tx, Waker, col, el, row};
// Linked only to union parley_engine's `complex-scripts` feature on (see
// Cargo.toml) — enables CJK text segmentation. Not called directly.
use parley_engine as _;
use soul_player_host::{
    BrowseAxis, Command, Event, PlayState, PlayerHandle, RepeatMode, ReplayGainMode, TrackView,
    WatchUpdate,
};

mod config;
#[cfg(test)]
mod i18n_tests;
mod theme;
mod ui;
mod update;
mod views;

use views::player::ArtCache;

/// The UI locales shipped besides the English fallback (see `locales/app.yml`).
pub(crate) const LOCALES: &[&str] = &["en", "de", "es", "zh", "cs", "ja"];

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
    Albums,
    Settings,
}

/// Which Settings category the content pane shows — a sub-navigation inside the
/// Settings route, mirroring the original soul-player's settings rail
/// (Appearance / Music Data / Audio / About). Ephemeral UI state (not persisted);
/// defaults to `Appearance` each launch.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum SettingsTab {
    Appearance,
    MusicData,
    Audio,
    About,
}

/// Light/dark preference: follow the OS, or force one. Persisted; the Appearance
/// settings section sets it.
#[derive(Clone, Copy, PartialEq, Debug)]
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
    /// The manual queue ("Next in queue"): tracks the user queued via
    /// `PlayNext`/`Enqueue`, in play order. Reorderable and removable.
    up_next: Vec<u32>,
    /// The real upcoming CONTEXT tracks (shuffle-correct) after the manual queue,
    /// from the backend's `QueueState` — the "Next from <context>" band. This
    /// replaces the old synthesis from `library` order (wrong under shuffle).
    context_ids: Vec<u32>,
    /// Transient drag-reorder state for the Up-Next band: `Some((from, to))` while
    /// a row is being dragged, `None` at rest. Drives the drop indicator; the
    /// commit (`Command::MoveInQueue`) fires on release.
    queue_drag: Option<(usize, usize)>,
    context_pos: u32,
    context_len: u32,
    /// Current page (Library vs Settings).
    route: Route,
    /// Which Settings category the content pane shows (ephemeral; defaults to
    /// `Appearance`). Switched by the settings page's own category rail.
    settings_tab: SettingsTab,
    /// Light/dark preference (persisted); `System` follows the OS.
    theme_pref: ThemePref,
    /// Which baked axis the library list is ordered by.
    sort_axis: BrowseAxis,
    /// The watched-folder set (from the host), rendered by Settings → Sources;
    /// `unavailable` are configured roots currently unreachable (shown "offline").
    watched_folders: Vec<PathBuf>,
    unavailable: Vec<PathBuf>,
    /// The chosen output device (persisted), rendered by Settings → Output;
    /// `None` = follow the system default. Applies to the next playback session.
    output_device: Option<String>,
    /// The output devices enumerated from the host at startup — the picker's
    /// choices. Empty on a host that exposes none (headless / no audio).
    output_devices: Vec<String>,
    /// ReplayGain normalization mode (persisted), rendered by Settings →
    /// ReplayGain; `Off` = no normalization.
    replaygain_mode: ReplayGainMode,
    /// ReplayGain preamp in dB (persisted), a constant offset on top of the mode's
    /// gain. Rendered as a slider in Settings → ReplayGain.
    replaygain_preamp: f32,
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
    /// Which in-app dropdown is open, by its key (`None` = none). Ephemeral UI
    /// state — the themed `views::dropdown` component reads it to show its panel,
    /// and a full-window backdrop closes it on an outside click.
    open_dropdown: Option<String>,
    /// The key of the slider being dragged right now (`None` = none), so it alone
    /// shows the horizontal-drag cursor. Ephemeral.
    dragging_slider: Option<String>,
    /// While the seek bar is being scrubbed, the previewed position in seconds —
    /// the knob and the elapsed-time label follow it, and the real `Seek` fires
    /// only on release. `None` when not scrubbing. Ephemeral.
    scrub_secs: Option<f64>,
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
        // Enumerate the output devices once (computed before `host` is moved into
        // the struct) for the Settings → Output picker.
        let output_devices = host.output_devices();
        // Restore the persisted ReplayGain choice (defaults: Off, 0 dB); main()
        // has already pushed the same value to the host before the first spawn.
        let (replaygain_mode, replaygain_preamp) = config::load_replaygain();
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
            context_ids: Vec::new(),
            queue_drag: None,
            context_pos: 0,
            context_len: 0,
            route: Route::Library,
            settings_tab: SettingsTab::Appearance,
            theme_pref: config::load_theme_pref()
                .map(|s| ThemePref::from_config(&s))
                .unwrap_or(ThemePref::System),
            sort_axis: BrowseAxis::ArtistAlbumTrack,
            watched_folders,
            unavailable,
            // Restore the persisted device choice (None = system default); the
            // enumerated list came from `host` just above.
            output_device: config::load_output_device(),
            output_devices,
            replaygain_mode,
            replaygain_preamp,
            art: ArtCache::new(),
            update_available: None,
            search: String::new(),
            searching: false,
            sidebar_width,
            sidebar_collapsed,
            locale_pref,
            open_dropdown: None,
            dragging_slider: None,
            scrub_secs: None,
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
                self.context_ids = q.context_ids.clone();
                self.context_pos = q.context_pos;
                self.context_len = q.context_len;
            }
            // Persisted off-thread in the subscribe closure (see `main`); the
            // hybrid-ReplayGain cache is silent — no UI state to fold in here.
            Event::TrackLoudnessAnalyzed { .. } => {}
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

/// Switch the active Settings category (ephemeral — not persisted). Used by the
/// settings page's category rail.
pub(crate) fn set_settings_tab(s: &mut State, tab: SettingsTab) {
    s.settings_tab = tab;
}

/// Open the dropdown with this key, or close it if it is already open (clicking
/// the trigger again). Only one dropdown is open at a time.
pub(crate) fn toggle_dropdown(s: &mut State, key: &str) {
    s.open_dropdown = if s.open_dropdown.as_deref() == Some(key) {
        None
    } else {
        Some(key.to_string())
    };
}

/// Close any open dropdown (an outside click, or after a choice is made).
pub(crate) fn close_dropdown(s: &mut State) {
    s.open_dropdown = None;
}

/// Choose + persist the output device (used by the Output settings buttons).
/// `None` means the system default. Updates the UI selection, writes the config,
/// and tells the host — which applies it to the NEXT playback session (a live
/// device switch mid-playback is a follow-up).
pub(crate) fn set_output_device(s: &mut State, name: Option<String>) {
    s.output_device = name.clone();
    config::save_output_device(s.output_device.as_deref());
    s.host.set_output_device(name);
}

/// Choose + persist the ReplayGain mode (used by the ReplayGain settings buttons).
/// Updates the UI selection, writes the config, and tells the host — which applies
/// it to the live actor immediately (and remembers it for the next session).
pub(crate) fn set_replaygain_mode(s: &mut State, mode: ReplayGainMode) {
    s.replaygain_mode = mode;
    config::save_replaygain(mode, s.replaygain_preamp);
    s.host.set_replay_gain(mode, s.replaygain_preamp);
}

/// Set + persist the ReplayGain preamp in dB (used by the preamp slider). Same
/// store-persist-tell-host path as [`set_replaygain_mode`].
pub(crate) fn set_replaygain_preamp(s: &mut State, preamp_db: f32) {
    s.replaygain_preamp = preamp_db;
    config::save_replaygain(s.replaygain_mode, preamp_db);
    s.host.set_replay_gain(s.replaygain_mode, preamp_db);
}

/// Switch the library's sort order — re-fetches the current library's views in
/// the chosen baked axis. Track ids are stable across axes (only the display +
/// play-context order changes), so playback is undisturbed.
pub(crate) fn set_sort(s: &mut State, axis: BrowseAxis) {
    s.sort_axis = axis;
    s.library = s.host.views_by(axis);
}

/// Set the output volume (0..1): clamp, update `State::volume` optimistically so
/// the UI (slider + %) tracks immediately, and tell the host. Optimistic because
/// scroll/keyboard nudges are incremental — the next one must read this new value,
/// not wait for the host's echo. Shared by the volume slider, the Up/Down keys,
/// and the scroll-over-volume gesture.
pub(crate) fn set_volume(s: &mut State, v: f32) {
    let v = v.clamp(0.0, 1.0);
    s.volume = v;
    s.host.send(Command::SetVolume(v));
}

fn view(s: &State, cx: &mut Cx<State>) -> El {
    let p = theme::palette(is_dark(s.theme_pref, cx.system_dark()));
    let scale = cx.env().scale;
    // The old app's shell: a fixed-width left sidebar (nav label, queue,
    // player panel) plus the main area's track list filling the rest.
    let shell = row()
        .w_full()
        .h_full()
        .fill(p.bg)
        .child(if s.sidebar_collapsed {
            views::sidebar::collapsed_strip(&p)
        } else {
            views::sidebar::sidebar(s, &p, cx, scale)
        })
        .child(views::sidebar::resize_handle(&p))
        .child(col().grow(1.0).min_h(0.0).child(match s.route {
            Route::Library => views::library::track_list(s, &p).grow(1.0),
            Route::Albums => views::albums::albums_page(s, &p, cx, scale).grow(1.0),
            Route::Settings => views::settings::settings_page(s, &p, cx).grow(1.0),
        }));
    // Overlay layer: mounted at the window root AFTER the shell so an open
    // dropdown's panel draws above everything and escapes the content scroll
    // viewport's clip (see `views::dropdown`). Empty when nothing is open.
    let overlays = views::settings::settings_overlays(s, &p, cx);
    el().key("app-root")
        .w_full()
        .h_full()
        .child(shell)
        .children(overlays)
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
        Key::Up => set_volume(s, s.volume + 0.05),
        Key::Down => set_volume(s, s.volume - 0.05),
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
fn scene_settings_music(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::MusicData;
}
fn scene_settings_audio(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Audio;
}
fn scene_settings_about(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::About;
}
/// German settings scenes: the longest-label locale we ship, for catching text
/// overflow in `--snap`. `apply_locale` (not `set_locale_pref`) so the snapshot
/// never writes the user's persisted language.
fn scene_settings_music_de(s: &mut State) {
    apply_locale("de");
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::MusicData;
}
fn scene_settings_audio_de(s: &mut State) {
    apply_locale("de");
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Audio;
}
fn scene_settings_appearance_de(s: &mut State) {
    apply_locale("de");
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Appearance;
}
/// Appearance with the language dropdown open, for snapshotting the themed panel.
fn scene_settings_lang_open(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Appearance;
    s.open_dropdown = Some("lang-picker".to_string());
}
/// Same, forced to the light (Day) palette, for a theme-robustness snapshot.
fn scene_settings_lang_open_light(s: &mut State) {
    s.theme_pref = ThemePref::Light;
    scene_settings_lang_open(s);
}
/// Same, in German — confirms the panel's localized "System" + native names.
fn scene_settings_lang_open_de(s: &mut State) {
    apply_locale("de");
    s.theme_pref = ThemePref::Dark;
    scene_settings_lang_open(s);
}
fn scene_albums(s: &mut State) {
    s.route = Route::Albums;
}
fn scene_search(s: &mut State) {
    s.search = "aria".to_string();
}
fn scene_no_match(s: &mut State) {
    s.search = "zzz-no-such-track".to_string();
}
fn scene_collapsed(s: &mut State) {
    s.sidebar_collapsed = true;
}
/// Seed a populated banded queue from the loaded library so `--snap` can capture
/// the "Up Next" sidebar (which is empty until playback otherwise).
fn scene_queue(s: &mut State) {
    let ids: Vec<u32> = s.library.iter().take(9).map(|t| t.id).collect();
    if let Some(&first) = ids.first() {
        s.now_playing_id = Some(first);
        s.playing = true;
        s.up_next = ids.iter().skip(1).take(2).copied().collect();
        s.context_ids = ids.iter().skip(3).copied().collect();
        s.context_pos = 0;
        s.context_len = s.library.len() as u32;
    }
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
    // Restore the persisted output-device choice BEFORE any autoload/open spawns
    // the actor, so the first playback session already targets the chosen device
    // (empty / absent config = system default).
    host.set_output_device(config::load_output_device());
    // Restore the persisted ReplayGain choice before any spawn, so the first
    // playback session is already normalized (the host banks it and tells the
    // actor on spawn).
    let (rg_mode, rg_preamp) = config::load_replaygain();
    host.set_replay_gain(rg_mode, rg_preamp);
    // Seed the hybrid-ReplayGain loudness cache from the persisted file (stale
    // entries already dropped on load) BEFORE any spawn, so a previously-analyzed
    // untagged track is normalized this session with no re-scan.
    host.seed_loudness(config::load_loudness_cache());
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
            ("settings-music", scene_settings_music),
            ("settings-audio", scene_settings_audio),
            ("settings-about", scene_settings_about),
            ("settings-music-de", scene_settings_music_de),
            ("settings-audio-de", scene_settings_audio_de),
            ("settings-appearance-de", scene_settings_appearance_de),
            ("settings-lang-open", scene_settings_lang_open),
            ("settings-lang-open-light", scene_settings_lang_open_light),
            ("settings-lang-open-de", scene_settings_lang_open_de),
            ("albums", scene_albums),
            ("search", scene_search),
            ("no-match", scene_no_match),
            ("collapsed", scene_collapsed),
            ("queue", scene_queue),
        ],
        ..AppSpec::new(
            "Soul Player",
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
        // Persist a completed background analysis to the on-disk cache (best-effort,
        // off the UI thread) so it seeds the NEXT launch; the fold into playback is
        // the backend's, already in its in-memory cache.
        if let Event::TrackLoudnessAnalyzed { path, gain_db } = &ev {
            config::record_loudness(path, *gain_db);
        }
        ah.update(move |s: &mut State, tx: &mut Tx| {
            s.apply(&ev);
            tx.instant(); // don't animate fast-moving numeric fields
        });
    });
    // Window + taskbar icon: the Soul Player logo, set at runtime (the packaged
    // installer uses the same icons via [package.metadata.packager]).
    if let Err(e) = rune::set_app_icon(include_bytes!("../icons/icon.png").to_vec()) {
        eprintln!("[icon] set_app_icon failed: {e}");
    }
    app.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    use rune::{Cursor, Env, Mods, Runtime, WheelEvent};

    use super::*;
    use crate::views::sidebar::{DEFAULT_WIDTH, MAX_WIDTH, MIN_WIDTH, STRIP_WIDTH};

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

    /// Click the element with `key`: find its position, drive a full
    /// move/down/up pointer gesture there, then re-render. The harness-level
    /// analogue of a real click, for e2e navigation tests.
    fn click_key(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt
            .find(key)
            .unwrap_or_else(|| panic!("no element with key {key:?} to click"));
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    /// Walk every laid-out node and return human-readable offenders: a visible
    /// node whose box leaves the window horizontally, or a text node an ancestor
    /// clips horizontally. Both are the signature of a label or action row a
    /// longer locale overflowed because it didn't wrap or shrink. We judge the
    /// horizontal axis only — vertical overflow is legitimate (the settings pane
    /// and the queue scroll), so comparing `rect` against `visible`/the window on
    /// x alone keeps scrolled content from reading as a failure.
    fn horizontal_overflows(rt: &Runtime<State>, viewport_w: f32) -> Vec<String> {
        const TOL: f32 = 1.0;
        let mut bad = Vec::new();
        for e in rt.elements() {
            // A node mid-exit or faded out is allowed off-stage.
            if e.exiting || e.opacity <= 0.01 {
                continue;
            }
            let [x, _y, w, _h] = e.rect;
            let id = e.key.clone().unwrap_or_else(|| e.path.clone());
            let shows = e.text.as_deref().unwrap_or("");
            // (a) the drawn box must stay inside the window horizontally.
            if x < -TOL || x + w > viewport_w + TOL {
                bad.push(format!(
                    "off-window [{id}] text={shows:?} x={x:.0} w={w:.0} (window {viewport_w:.0})"
                ));
                continue;
            }
            // (b) text must not be horizontally clipped by an ancestor: `visible`
            // (what ancestors' clips leave) must still span the drawn width.
            if !shows.is_empty() {
                let [vx, _vy, vw, _vh] = e.visible;
                if vx > x + TOL || vw + TOL < w {
                    bad.push(format!(
                        "clipped text [{id}] {shows:?} rect_w={w:.0} visible_w={vw:.0}"
                    ));
                }
            }
        }
        bad
    }

    /// Settings must survive every shipped locale without a label or action row
    /// overflowing the window or being clipped — German and Spanish run far
    /// longer than the English the layout was first sized against. Renders each
    /// category in each locale through the real `view` and asserts the layout
    /// contains itself. This is the regression net for i18n text robustness.
    #[test]
    fn settings_layout_contains_itself_in_every_locale() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let tabs = [
            ("Appearance", SettingsTab::Appearance),
            ("MusicData", SettingsTab::MusicData),
            ("Audio", SettingsTab::Audio),
            ("About", SettingsTab::About),
        ];
        // A roomy window and a cramped one: the narrow pass is where button rows
        // (theme / language / ReplayGain) must wrap rather than overflow.
        let viewports = [(1100.0_f32, 720.0_f32), (900.0, 640.0)];
        // One watched folder so the Music Data source row (name + remove ✕) is
        // exercised too, not just the empty state.
        let folder = testdata_dir().join("a-reasonably-long-source-folder-name");
        let mut failures = Vec::new();
        for &viewport in &viewports {
            for &loc in LOCALES {
                for (tab_name, tab) in tabs {
                    let mut s = settings_state(std::slice::from_ref(&folder), &[]);
                    s.settings_tab = tab;
                    // AFTER State::new, which re-applies the persisted locale over ours.
                    apply_locale(loc);
                    let mut rt = Runtime::new(s, view, viewport, env);
                    rt.frame(0.0);
                    for bad in horizontal_overflows(&rt, viewport.0) {
                        failures.push(format!("[{loc}/{tab_name}@{}] {bad}", viewport.0));
                    }
                }
            }
        }
        apply_locale("en"); // don't leak the locale into sibling tests
        assert!(
            failures.is_empty(),
            "{} settings layout overflow(s) across locales:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// A library scanned from the committed testdata, so Library / Albums / the
    /// queue render real rows (not just empty states) for the layout sweep.
    fn populated_state() -> State {
        let mut s = State::new(PlayerHandle::offline());
        if let Ok(up) = s.host.add_folder(&testdata_dir()) {
            apply_watch(&mut s, up);
        }
        s
    }

    /// The same contain-itself invariant as settings, but swept across the whole
    /// app — Library, Albums and a populated queue/now-playing panel — in every
    /// locale at two widths. This is the general i18n-overflow net; new views get
    /// coverage by adding a scene here.
    #[test]
    fn app_layout_contains_itself_in_every_locale() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let viewports = [(1100.0_f32, 720.0_f32), (900.0, 640.0)];
        // Name → state tweak, the same `fn(&mut State)` shape as a `--scene`.
        type NamedScene = (&'static str, fn(&mut State));
        let scenes: [NamedScene; 4] = [
            ("library", |s: &mut State| s.route = Route::Library),
            ("albums", |s: &mut State| s.route = Route::Albums),
            // Populates now-playing + both queue bands from the library; the
            // sidebar panel then renders over whatever route is active.
            ("queue", scene_queue),
            ("settings-music", |s: &mut State| {
                s.route = Route::Settings;
                s.settings_tab = SettingsTab::MusicData;
            }),
        ];
        let mut failures = Vec::new();
        for &viewport in &viewports {
            for &loc in LOCALES {
                for (scene_name, scene) in scenes {
                    let mut s = populated_state();
                    scene(&mut s);
                    // AFTER State::new, which re-applies the persisted locale.
                    apply_locale(loc);
                    let mut rt = Runtime::new(s, view, viewport, env);
                    rt.frame(0.0);
                    for bad in horizontal_overflows(&rt, viewport.0) {
                        failures.push(format!("[{loc}/{scene_name}@{}] {bad}", viewport.0));
                    }
                }
            }
        }
        apply_locale("en");
        assert!(
            failures.is_empty(),
            "{} app layout overflow(s) across locales:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// The language picker is the themed `views::dropdown`: pressing the trigger
    /// opens a floating panel (mounted at the root, escaping the settings scroll
    /// clip), the choices render, and picking one commits the locale + closes.
    #[test]
    fn language_dropdown_opens_renders_and_picks() {
        // Picking flows through `set_locale_pref`, which writes config — isolate it.
        config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::Appearance;
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Closed: the trigger renders, no panel.
        assert!(
            rt.find("lang-picker").is_some(),
            "the language dropdown trigger renders"
        );
        assert!(
            rt.find("lang-picker-menu").is_none(),
            "the panel is closed initially"
        );
        // Open it, then settle the panel's enter transition (it fades/scales in
        // from opacity 0, so it isn't hit-testable until the animation finishes).
        click_key(&mut rt, "lang-picker");
        rt.settle_all();
        rt.frame(0.0);
        assert_eq!(rt.state.open_dropdown.as_deref(), Some("lang-picker"));
        assert!(
            rt.find("lang-picker-menu").is_some(),
            "pressing the trigger opens the panel"
        );
        assert!(
            rt.find("lang-picker-opt-de").is_some(),
            "the panel lists the language choices"
        );
        // Pick German: commits the locale and closes.
        click_key(&mut rt, "lang-picker-opt-de");
        assert_eq!(rt.state.locale_pref, "de", "picking commits the locale");
        assert_eq!(rt.state.open_dropdown, None, "picking closes the dropdown");

        apply_locale("en"); // reset the shared rust_i18n locale for sibling tests
    }

    /// Architecture: `views::dropdown` is the ONE way to build a dropdown. No view
    /// may fall back to rune's native picker (`PopupMenu::picker`), which draws an
    /// OS-styled menu that clashes with the app's look. Native `popup_menu`
    /// CONTEXT menus (library/queue right-click) are a different control and stay
    /// allowed — this bans only the picker/select constructor. New pickers go
    /// through `dropdown::trigger` + `dropdown::panel`.
    #[test]
    fn dropdowns_only_use_the_shared_component() {
        let views_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/views");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&views_dir).expect("read views dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let src = std::fs::read_to_string(&path).expect("read view source");
            for (i, line) in src.lines().enumerate() {
                // Only code, not doc/comments that may name the banned API.
                let code = line.split("//").next().unwrap_or("");
                if code.contains("PopupMenu::picker") || code.contains(".picker(") {
                    offenders.push(format!("{name}:{} {}", i + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "dropdowns must use views::dropdown, not rune's native PopupMenu::picker:\n{}",
            offenders.join("\n")
        );
    }

    /// A slider shows the horizontal-drag cursor only while it is actually being
    /// dragged — hovering keeps the plain cursor, so the change reads as "you are
    /// dragging" rather than a persistent hint.
    #[test]
    fn slider_shows_a_drag_cursor_only_while_dragging() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.volume = 0.5;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("volume").expect("the volume slider renders");
        // Hover: plain cursor.
        rt.pointer_move(x, y);
        rt.frame(0.0);
        assert_eq!(
            rt.cursor(),
            Cursor::Default,
            "hovering the slider keeps the default cursor"
        );
        // Press + move = a drag: the horizontal-resize cursor appears.
        rt.pointer_down(x, y);
        rt.pointer_move(x + 12.0, y);
        rt.frame(0.0);
        assert_eq!(rt.state.dragging_slider.as_deref(), Some("volume"));
        assert_eq!(
            rt.cursor(),
            Cursor::ResizeColumn,
            "dragging the slider shows the horizontal-drag cursor"
        );
        // Release clears the drag state.
        rt.pointer_up(x + 12.0, y);
        rt.frame(0.0);
        assert_eq!(rt.state.dragging_slider, None);
    }

    /// Dragging the seek bar scrubs a live preview (knob + elapsed time follow the
    /// pointer) without seeking on every move, then commits one Seek on release.
    #[test]
    fn dragging_the_seek_bar_scrubs_then_commits_on_release() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        s.duration_secs = 100.0;
        s.position_secs = 10.0;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("seek").expect("the seek slider renders");
        // Drag toward the right: a scrub preview appears and moves forward, with
        // no seek committed mid-drag.
        rt.pointer_down(x, y);
        rt.pointer_move(x + 40.0, y);
        rt.frame(0.0);
        let scrub = rt
            .state
            .scrub_secs
            .expect("dragging previews a scrub position");
        assert!(scrub > 10.0, "scrubbed forward from 10s, got {scrub}");
        // Release ends the scrub (the Seek command goes to the host).
        rt.pointer_up(x + 40.0, y);
        rt.frame(0.0);
        assert_eq!(
            rt.state.scrub_secs, None,
            "releasing ends the scrub preview"
        );
    }

    /// Scrolling the wheel over the volume row nudges the volume — up (dy < 0)
    /// louder, down quieter — the end-to-end path through `on_wheel` →
    /// `wheel_volume_delta` → `set_volume`. Starts mid-range so both directions
    /// move (the default 1.0 is already at the ceiling).
    #[test]
    fn scrolling_over_the_volume_row_changes_the_volume() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false; // the player panel (volume row) must render
        s.volume = 0.5;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (x, y) = rt.find("volume-row").expect("the volume row renders");
        // Scroll up → one 5% step louder.
        rt.wheel_event(WheelEvent::new(0.0, -10.0).at(x, y));
        rt.frame(0.0);
        assert!(
            (rt.state.volume - 0.55).abs() < 1e-6,
            "scroll up raised the volume, got {}",
            rt.state.volume
        );
        // Scroll down → back to 0.5.
        rt.wheel_event(WheelEvent::new(0.0, 10.0).at(x, y));
        rt.frame(0.0);
        assert!(
            (rt.state.volume - 0.50).abs() < 1e-6,
            "scroll down lowered the volume, got {}",
            rt.state.volume
        );
    }

    #[test]
    fn settings_route_shows_sources_section_and_add_button() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
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

    // ── Sidebar resize & collapsed-mode e2e, driven through the real pointer ──
    // gestures and measured against the original soul-player's live
    // `useCollapsibleSidebar` + `CollapsedSidebarStrip`. Parity asserted here
    // (original → ours): width range 240–480 default 288; collapse snap < 200 px;
    // collapsed rail 12 px (`w-3`) with a 4×56 px grabber (`w-1 h-14`), centred;
    // `cursor-ew-resize` on both the handle and the rail; and a collapse that
    // preserves the expanded width (the original never overwrites `sidebar-width`
    // on snap, so re-opening restores it).
    //
    // Two intentional divergences, documented so a future reader doesn't "fix"
    // them into regressions: (1) ours resizes by pointer *delta*
    // (`width_at_press + dx`) rather than the original's absolute
    // `width = clientX`, so grabbing the handle never jumps the edge to the
    // cursor; (2) there is no 20 px re-open hysteresis — we re-open the instant
    // the drag target clears 200 px where the original waits for 220 px.
    //
    // Every drag deliberately stops at `Phase::Move` and never calls
    // `pointer_up`: `config::save_sidebar` only fires on release, so no test
    // here writes the user's real `config.toml`.

    /// A framed headless runtime whose sidebar starts at an explicit geometry,
    /// overriding whatever the user's persisted `config.toml` holds so the test
    /// is deterministic (as the other sidebar tests do). One frame is pumped so
    /// `find`/`presented` have layout.
    fn sidebar_rt(width: f32, collapsed: bool) -> Runtime<State> {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_width = width;
        s.sidebar_collapsed = collapsed;
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
        rt
    }

    #[test]
    fn expanded_sidebar_draws_at_its_configured_width() {
        // The old app applies the width via an inline `style={{ width }}`; ours
        // binds `.w(s.sidebar_width)`. Prove a non-default 360 px reaches the
        // drawn element (not merely the state), and that the right edge offers
        // the resize handle's `cursor-ew-resize`.
        let mut rt = sidebar_rt(360.0, false);
        let drawn = rt.presented("sidebar").expect("sidebar renders").0;
        assert!(
            (drawn[2] - 360.0).abs() < 0.5,
            "the sidebar draws at its configured width, got {}",
            drawn[2]
        );
        let (hx, hy) = rt.find("sidebar-resize").expect("handle present");
        rt.pointer_move(hx, hy);
        rt.frame(0.0);
        assert_eq!(
            rt.cursor(),
            Cursor::ResizeColumn,
            "the right-edge handle shows the resize cursor"
        );
    }

    #[test]
    fn dragging_the_handle_resizes_within_the_min_max_range() {
        // One captured drag with several moves — the e2e twin of the pure
        // `resize_to` unit test. `Drag::data` caches 288 at press; each move's
        // `dx` is travel from the press point.
        let mut rt = sidebar_rt(DEFAULT_WIDTH, false);
        let (hx, hy) = rt.find("sidebar-resize").expect("handle present");
        rt.pointer_move(hx, hy);
        rt.pointer_down(hx, hy);
        // Pointer events dispatch on the next `frame`, so each move is applied by
        // the `frame` that follows it. `Drag::data` (288, cached at press) and the
        // press origin both persist across the gesture, so every `dx` below is
        // travel from the original press point.
        // Out by 100 → 388, inside the range.
        rt.pointer_move(hx + 100.0, hy);
        rt.frame(0.0);
        assert_eq!(
            rt.state.sidebar_width, 388.0,
            "drag-out widens by the pointer travel"
        );
        // Keep pulling out hard → clamps at MAX, never wider.
        rt.pointer_move(hx + 400.0, hy);
        rt.frame(0.0);
        assert_eq!(rt.state.sidebar_width, MAX_WIDTH, "clamps to the max width");
        // Back in, but still above the collapse threshold (288 - 60 = 228) →
        // clamps up to MIN rather than collapsing.
        rt.pointer_move(hx - 60.0, hy);
        rt.frame(0.0);
        assert_eq!(rt.state.sidebar_width, MIN_WIDTH, "clamps to the min width");
        assert!(
            !rt.state.sidebar_collapsed,
            "stays expanded while above the threshold"
        );
    }

    #[test]
    fn dragging_the_handle_below_the_threshold_collapses_to_the_rail() {
        let mut rt = sidebar_rt(DEFAULT_WIDTH, false);
        let (hx, hy) = rt.find("sidebar-resize").expect("handle present");
        rt.pointer_move(hx, hy);
        rt.pointer_down(hx, hy);
        // 288 - 150 = 138 < 200 → snap collapsed (applied on the next frame).
        rt.pointer_move(hx - 150.0, hy);
        rt.frame(0.0);
        assert!(
            rt.state.sidebar_collapsed,
            "dragging below the 200 px threshold snaps collapsed"
        );
        assert_eq!(
            rt.state.sidebar_width, DEFAULT_WIDTH,
            "a collapse preserves the expanded width for re-opening"
        );
        rt.frame(0.0);
        assert!(
            rt.find("sidebar-collapsed").is_some(),
            "the thin rail replaces the full sidebar"
        );
        assert!(rt.find("sidebar").is_none(), "the full sidebar is gone");
        assert!(
            rt.find("sidebar-resize").is_some(),
            "the handle stays, so one gesture can drag back out"
        );
    }

    #[test]
    fn dragging_out_from_the_collapsed_rail_reopens_the_sidebar() {
        let mut rt = sidebar_rt(DEFAULT_WIDTH, true);
        let (hx, hy) = rt
            .find("sidebar-resize")
            .expect("handle present beside the rail");
        rt.pointer_move(hx, hy);
        // Phase::Down caches the rail width (STRIP_WIDTH) as the drag origin.
        rt.pointer_down(hx, hy);
        // 12 + 250 = 262 ≥ 200 → re-opens to the dragged width (next frame).
        rt.pointer_move(hx + 250.0, hy);
        rt.frame(0.0);
        assert!(
            !rt.state.sidebar_collapsed,
            "dragging out past the threshold re-opens the sidebar"
        );
        assert_eq!(
            rt.state.sidebar_width,
            STRIP_WIDTH + 250.0,
            "re-opens to the dragged width"
        );
        rt.frame(0.0);
        assert!(rt.find("sidebar").is_some(), "the full sidebar is back");
        assert!(
            rt.find("sidebar-collapsed").is_none(),
            "the collapsed rail is gone"
        );
    }

    #[test]
    fn collapsed_rail_is_a_thin_strip_with_a_centered_grabber() {
        let mut rt = sidebar_rt(DEFAULT_WIDTH, true);
        // The rail is the old app's `w-3` (12 px) CollapsedSidebarStrip.
        let rail = rt.presented("sidebar-collapsed").expect("rail renders").0;
        assert!(
            (rail[2] - STRIP_WIDTH).abs() < 0.5,
            "the collapsed rail is a 12 px strip, got {}",
            rail[2]
        );
        // Its grabber pill is the old app's `w-1 h-14` → 4 × 56 px.
        let pill = rt.presented("grabber").expect("grabber renders").0;
        assert!(
            (pill[2] - 4.0).abs() < 0.5,
            "the grabber is 4 px wide, got {}",
            pill[2]
        );
        assert!(
            (pill[3] - 56.0).abs() < 0.5,
            "the grabber is 56 px tall, got {}",
            pill[3]
        );
        // Centred within the rail on BOTH axes (horizontal and vertical).
        let (rail_cx, rail_cy) = rt.find("sidebar-collapsed").unwrap();
        let (pill_cx, pill_cy) = rt.find("grabber").unwrap();
        assert!(
            (pill_cx - rail_cx).abs() < 0.5,
            "the grabber is centred horizontally in the rail"
        );
        assert!(
            (pill_cy - rail_cy).abs() < 1.0,
            "the grabber is centred vertically in the rail (not pinned to the top)"
        );
        // The rail itself offers the resize cursor (old app's `cursor-ew-resize`),
        // sampled above the pill so the pointer is over bare rail.
        rt.pointer_move(rail_cx, 120.0);
        rt.frame(0.0);
        assert_eq!(
            rt.cursor(),
            Cursor::ResizeColumn,
            "the collapsed rail shows the resize cursor"
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
        let mut s = settings_state(std::slice::from_ref(&dir), &[]);
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
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
        let mut s = settings_state(
            std::slice::from_ref(&missing),
            std::slice::from_ref(&missing),
        );
        s.settings_tab = SettingsTab::MusicData;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
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
    fn settings_shows_replaygain_section_with_modes_and_preamp() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::Audio;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("replaygain").is_some(),
            "Settings has a ReplayGain section"
        );
        for key in ["rg-off", "rg-track", "rg-album", "rg-preamp"] {
            assert!(rt.find(key).is_some(), "ReplayGain control {key} present");
        }
    }

    #[test]
    fn settings_shows_updates_section_without_install_by_default() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::About;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("updates").is_some(),
            "Settings has an About/Updates section"
        );
        assert!(
            rt.find("install-update").is_none(),
            "no install button until the launch check flags an update"
        );
    }

    #[test]
    fn settings_offers_install_when_update_available() {
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::About;
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
    fn settings_nav_lists_categories_and_defaults_to_appearance() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        for key in ["cat-appearance", "cat-music-data", "cat-audio", "cat-about"] {
            assert!(rt.find(key).is_some(), "settings category rail has {key}");
        }
        // The default category is Appearance (theme + language cards); the Audio
        // category's ReplayGain card isn't rendered until it's selected.
        assert!(
            rt.find("appearance").is_some(),
            "default category shows the Appearance card"
        );
        assert!(
            rt.find("language").is_some(),
            "Appearance also hosts the Language card"
        );
        assert!(
            rt.find("replaygain").is_none(),
            "ReplayGain lives under the Audio category, not the default view"
        );
    }

    #[test]
    fn clicking_settings_categories_switches_the_content() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Default category is Appearance.
        assert!(rt.find("appearance").is_some());
        assert!(rt.find("replaygain").is_none());
        // Click Audio → the Output + ReplayGain cards appear, Appearance hides.
        click_key(&mut rt, "cat-audio");
        assert!(rt.find("output").is_some(), "Audio shows the Output card");
        assert!(
            rt.find("replaygain").is_some(),
            "Audio shows the ReplayGain card"
        );
        assert!(
            rt.find("appearance").is_none(),
            "the Appearance card is hidden under Audio"
        );
        // Click Music Data → the sources card (keyed "sources").
        click_key(&mut rt, "cat-music-data");
        assert!(
            rt.find("sources").is_some(),
            "Music Data shows the sources card"
        );
        assert!(rt.find("replaygain").is_none());
        // Click About → the version / updater card (keyed "updates").
        click_key(&mut rt, "cat-about");
        assert!(rt.find("updates").is_some(), "About shows the version card");
        assert!(rt.find("sources").is_none());
        // Back to Appearance.
        click_key(&mut rt, "cat-appearance");
        assert!(rt.find("appearance").is_some(), "back to Appearance");
    }

    #[test]
    fn settings_controls_apply_their_effect_on_click() {
        // These clicks persist (theme/ReplayGain `save_*`) — isolate config to a
        // temp dir so the test never clobbers the user's real settings.
        config::use_test_config_dir();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        // Appearance (the default tab): clicking a theme updates the preference.
        let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert_eq!(rt.state.settings_tab, SettingsTab::Appearance);
        click_key(&mut rt, "theme-dark");
        assert_eq!(rt.state.theme_pref, ThemePref::Dark);
        click_key(&mut rt, "theme-light");
        assert_eq!(rt.state.theme_pref, ThemePref::Light);

        // Audio controls: start on the Audio tab so the ReplayGain buttons are
        // laid out, then clicking a mode flows through to the state.
        let mut s = settings_state(&[], &[]);
        s.settings_tab = SettingsTab::Audio;
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        click_key(&mut rt, "rg-track");
        assert_eq!(rt.state.replaygain_mode, ReplayGainMode::Track);
        click_key(&mut rt, "rg-album");
        assert_eq!(rt.state.replaygain_mode, ReplayGainMode::Album);
        click_key(&mut rt, "rg-off");
        assert_eq!(rt.state.replaygain_mode, ReplayGainMode::Off);
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

    /// A populated queue renders both bands — the manual "Next in queue" (with a
    /// drag grip, remove ✕, and Clear) and the "Next from <context>" remainder.
    #[test]
    fn populated_queue_renders_both_bands_with_controls() {
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        s.sidebar_collapsed = false; // deterministic — don't depend on persisted config
        let ids: Vec<u32> = s.library.iter().take(5).map(|t| t.id).collect();
        assert!(ids.len() >= 5, "testdata has enough tracks for the bands");
        s.now_playing_id = Some(ids[0]);
        s.up_next = ids[1..3].to_vec(); // two manual rows
        s.context_ids = ids[3..].to_vec(); // context rows
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("queue-next-in-queue").is_some(),
            "the manual band header renders"
        );
        assert!(
            rt.find("queue-next-from").is_some(),
            "the context band header renders"
        );
        assert!(rt.find("clear-queue").is_some(), "Clear is offered");
        let m0 = ids[1];
        assert!(
            rt.find(&format!("queue-manual-{m0}")).is_some(),
            "a manual queue row renders"
        );
        assert!(
            rt.find(&format!("grip-{m0}")).is_some(),
            "the manual row has a drag grip"
        );
        assert!(
            rt.find(&format!("remove-{m0}")).is_some(),
            "the manual row has a remove control"
        );
        let c0 = ids[3];
        assert!(
            rt.find(&format!("queue-context-{c0}")).is_some(),
            "a context row renders"
        );
        assert!(
            rt.find(&format!("grip-{c0}")).is_none(),
            "context rows are not reorderable (no grip)"
        );
    }

    /// Nothing queued and nothing upcoming shows the empty state, not stale rows.
    #[test]
    fn empty_queue_shows_the_empty_state() {
        let mut s = State::new(PlayerHandle::offline());
        s.sidebar_collapsed = false;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("queue-empty").is_some(),
            "an empty queue shows the empty state"
        );
    }

    /// Drag-reorder, driven through the real pointer: pressing a manual row's grip
    /// picks it up, dragging down past a row height advances the drop target and
    /// shows the drop indicator, and releasing ends the gesture (committing the
    /// move). The same `Runtime` + `pointer_*` harness the sidebar resize uses.
    #[test]
    fn dragging_a_queue_row_shows_a_drop_target_then_commits_on_release() {
        use crate::views::queue::QUEUE_ROW_H;
        let host = PlayerHandle::offline();
        host.open_folder(&testdata_dir()).expect("scan testdata");
        let mut s = State::new(host);
        s.sidebar_collapsed = false;
        let ids: Vec<u32> = s.library.iter().take(6).map(|t| t.id).collect();
        assert!(ids.len() >= 6, "testdata has enough tracks");
        s.now_playing_id = Some(ids[0]);
        s.up_next = ids[1..4].to_vec(); // 3 manual rows at indices 0, 1, 2
        s.context_ids = ids[4..].to_vec();
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);

        // Press the first manual row's grip and drag it down ~1.5 row heights.
        let (gx, gy) = rt
            .find(&format!("grip-{}", ids[1]))
            .expect("the first manual row has a drag grip");
        rt.pointer_move(gx, gy);
        rt.pointer_down(gx, gy);
        rt.pointer_move(gx, gy + QUEUE_ROW_H * 1.5);
        rt.frame(0.0);
        assert_eq!(
            rt.state.queue_drag.map(|(from, _)| from),
            Some(0),
            "pressing a grip picks up that row"
        );
        assert!(
            rt.state.queue_drag.map(|(_, to)| to).unwrap_or(0) >= 1,
            "dragging down advances the drop target"
        );
        assert!(
            rt.find("drop-line").is_some(),
            "a drop indicator shows while dragging"
        );

        // Release ends the gesture and clears the transient drag state.
        rt.pointer_up(gx, gy + QUEUE_ROW_H * 1.5);
        rt.frame(0.0);
        assert_eq!(rt.state.queue_drag, None, "releasing ends the drag");
        assert!(
            rt.find("drop-line").is_none(),
            "the drop indicator is gone once released"
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
