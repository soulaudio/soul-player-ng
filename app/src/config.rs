//! Settings persistence: one structured `config.toml` in the user's config dir,
//! (de)serialised via serde, the best-practice shape (a single typed file in the
//! OS config dir) replacing the earlier scattered plain-text files, which this
//! migrates from on first run. App-level on purpose: it keeps `soul-player-host`
//! pure (no I/O, no config-path knowledge). Best-effort throughout: a missing or
//! unreadable config loads as defaults, and a failed save never breaks the UI.
//!
//! The per-setting `load_*`/`save_*` API is kept; each `save_*` does a
//! load-modify-write of the one file (it's tiny, and the UI is single-threaded).
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use soul_player_host::{
    Band, BandKind, DspChain, DspUnit, EqUnit, MAX_BANDS, Profile, ProfileSet, ReplayGainMode,
    SCAN_CACHE_VERSION, ScanCache, TrackLoudness, default_bands,
};
// The persisted profile/chain exchange format now lives in the shared `soul-audio-dsp`
// crate (re-exported through the host), so the DAP reads the identical shapes. This
// module keeps only the file I/O over them.
use soul_player_host::format::{BandCfg, DspUnitCfg, ProfileCfg};

use crate::views::sidebar::{DEFAULT_WIDTH, MAX_WIDTH, MIN_WIDTH};

/// The throwaway per-process config directory used in test builds, so `cargo
/// test` can NEVER read or write the user's real settings, regardless of
/// whether a test opts in. Compiled out of production.
#[cfg(test)]
static TEST_CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// A process-wide lock serialising EVERY test that persists config. The test
/// config dir is shared per-process, and `save_*` does a load-modify-write of the
/// whole `config.toml`, so two config-writing tests running concurrently clobber
/// each other's fields. Any test that mutates+reads config must hold this for its
/// duration, which is exactly what [`use_test_config_dir`] now returns.
#[cfg(test)]
static CONFIG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Isolate a test from the shared config file: returns a guard on the process-wide
/// [`CONFIG_TEST_LOCK`], so config-mutating tests run one at a time (no concurrent
/// load-modify-write clobbering). Bind it for the test's duration
/// (`let _g = config::use_test_config_dir();`). Poisoning is ignored (one failing
/// test must not cascade into the rest). The temp dir itself is always isolated
/// (see [`soul_player_dir`]); this adds the serialization.
#[cfg(test)]
pub(crate) fn use_test_config_dir() -> std::sync::MutexGuard<'static, ()> {
    let guard = CONFIG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = soul_player_dir();
    guard
}

/// A process-wide lock serialising every test whose assertions depend on the
/// GLOBAL i18n locale. `rust_i18n::set_locale` sets a single process-wide
/// locale, so a test that sweeps non-"en" locales (the per-locale layout
/// overflow tests) running concurrently with one asserting a specific
/// localised string (e.g. a "Streams from …" tooltip) lets the sweeper flip
/// the locale mid-frame of the asserting test, the i18n test flake. Any test
/// that sets a non-default locale OR asserts localised text must hold this.
#[cfg(test)]
static LOCALE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Isolate a test from the shared global locale: acquire the process-wide
/// [`LOCALE_TEST_LOCK`] and set `locale`, returning the guard. Bind it for the
/// test's duration (`let _locale = config::use_test_locale("en");`) so no
/// concurrent locale-sensitive test can observe or clobber the locale while it
/// renders and asserts. A sweeper may set further locales while holding the
/// guard. Poisoning is ignored (one failing test must not cascade). Acquire
/// this BEFORE [`use_test_config_dir`] when a test needs both, so the two locks
/// are always taken in the same order (no deadlock).
#[cfg(test)]
pub(crate) fn use_test_locale(locale: &str) -> std::sync::MutexGuard<'static, ()> {
    let guard = LOCALE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    rust_i18n::set_locale(locale);
    guard
}

/// The app's config directory. In a **test build this is ALWAYS a throwaway
/// temp directory** (never the user's real config), tests share it, so a test
/// that persists a setting must not let another's write leak into its
/// assertions (set the relevant `State` field explicitly instead of trusting a
/// loaded value). In production it is `<os-config>/soul-player`.
///
/// `pub(crate)`: the playlist store (`playlists::store`) falls back to
/// `<this>/playlists` when no managed-library root is configured, so it needs
/// the same test/production split every other per-setting path here gets.
#[cfg(test)]
pub(crate) fn soul_player_dir() -> Option<PathBuf> {
    Some(
        TEST_CONFIG_DIR
            .get_or_init(|| {
                std::env::temp_dir().join(format!("soul-player-test-{}", std::process::id()))
            })
            .clone(),
    )
}

#[cfg(not(test))]
pub(crate) fn soul_player_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("soul-player"))
}

/// `<config>/soul-player/config.toml`: the one settings file.
fn config_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("config.toml"))
}

/// The on-disk settings. `#[serde(default)]` fills any field a config written by
/// an older build omits, so new settings never break an existing file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    /// Watched music folders (stored as strings, `PathBuf` isn't TOML-native).
    sources: Vec<String>,
    /// Persisted remote-source server base URLs (no trailing slash), so a
    /// restart can reconnect them. The OAuth refresh token itself is never
    /// stored here, it lives in the OS keychain, keyed by the same URL (see
    /// `soul_player_host::remote::KeyringStore`). A config written before
    /// remote sources existed loads this as empty (struct-level
    /// `#[serde(default)]`).
    remotes: Vec<String>,
    /// Server URLs (a subset of `remotes`) the user has disabled. Absent in
    /// old configs → everything enabled, the same "store the *disabled*
    /// subset" convention `sources_disabled` uses for local folders, so a
    /// remote added before this field existed loads enabled.
    #[serde(default)]
    remotes_disabled: Vec<String>,
    /// Theme preference: `system` | `light` | `dark`.
    theme: String,
    /// Resizable sidebar width (clamped to the legal range on load).
    sidebar_width: f32,
    /// Whether the sidebar is collapsed to its rail.
    sidebar_collapsed: bool,
    /// Content-pane zoom factor (Ctrl +/-/0), 1.0 = 100%. Clamped on load.
    ui_zoom: f32,
    /// UI language preference: `system` | `en` | `de` | `es` | `zh` | `cs`.
    locale: String,
    /// Primary nav items the user has hidden (by id: `home`/`albums`/`artists`/
    /// `tracks`). Empty = all shown. Storing the *hidden* set means a new nav
    /// item added in a later build defaults to visible.
    nav_hidden: Vec<String>,
    /// Paths (a subset of `sources`) the user has disabled. Absent in old configs
    /// → everything enabled. Storing the *disabled* subset keeps migration free.
    sources_disabled: Vec<String>,
    /// Managed-library destination folder (empty = unset). The write-path
    /// organizer copies / moves added files here.
    managed_library_path: String,
    /// Managed-library template PRESET id (e.g. `"artist-album"`): the folder
    /// layout the organizer arranges files into. Stored as the preset id, not the
    /// raw template string, so a later build can tweak a preset's template freely.
    managed_library_template: String,
    /// Managed-library file strategy: `"copy"` (keep the source) or `"move"`.
    managed_library_strategy: String,
    /// The DSP chain: an ordered list of typed DSP units the signal flows through
    /// (EQ is the only kind today). A future "audio profile" can bundle this whole
    /// list. Empty + legacy EQ fields present → migrated to a single unit on load.
    /// Kept under the on-disk key `eq_units` so configs from before the typed-unit
    /// migration load unchanged.
    eq_units: Vec<DspUnitCfg>,
    /// Legacy single-EQ fields (pre-chain), kept for one-time migration into
    /// `eq_units`. New saves leave them at defaults.
    eq_enabled: bool,
    eq_preamp: f32,
    eq_bands: Vec<BandCfg>,
    /// Library "indexing speed": `auto` | `max` | `gentle`. Controls how much
    /// parallelism the scanner uses; `auto` follows live CPU load.
    indexing_mode: String,
    /// Background loudness analysis mode: `off` | `throttled` | `full`. Controls the
    /// scan-time Tier-2 loudness sweep that fills the library loudness/DR column.
    loudness_analysis: String,
    /// Saved audio profiles: each a named snapshot of the whole DSP chain. The
    /// *active* profile's chain is the live one (and is mirrored into `eq_units`
    /// above for backward-compat). Empty in configs written before profiles existed
    /// → migrated to one `"Default"` profile from `eq_units` on load.
    profiles: Vec<ProfileCfg>,
    /// Index of the active profile within `profiles` (clamped on load).
    active_profile: usize,
    /// Last window geometry, restored on the next launch. Absent on a fresh
    /// install → the window opens at its default placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window: Option<WindowGeometry>,
    /// Which date the UI treats as an album's "year" (display + discography
    /// sort): the original first-release date, or this particular release's
    /// own date. `#[serde(default)]` so a config written before this setting
    /// existed loads as `Original` (the field-level default below mirrors the
    /// struct-level one for clarity).
    #[serde(default)]
    album_year_source: AlbumYearSource,
    /// Whether the library track list shows the per-track loudness columns (DR +
    /// integrated LUFS). `#[serde(default = "default_true")]` so a config written
    /// before this setting loads as shown, the long-standing behaviour (a bare
    /// `#[serde(default)]` would wrongly default a missing bool to `false`).
    #[serde(default = "default_true")]
    show_loudness_columns: bool,
    /// Settings → Updates: check for updates at launch and on a schedule. Default
    /// on (`default_true`, so a config from before this setting keeps checking).
    #[serde(default = "default_true")]
    update_auto_check: bool,
    /// Settings → Updates: download a found update in the background and install
    /// it when the app is next quit. Opt-in (default off).
    #[serde(default)]
    update_auto_install: bool,
    /// macOS: the bundle path the user chose to keep the app at ("Keep Here" on
    /// the Move to Applications prompt), so that folder isn't asked about again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    move_prompt_kept: Option<String>,
}

/// serde field default for a bool that should default to `true` when absent from an
/// older config (plain `#[serde(default)]` gives `false`).
fn default_true() -> bool {
    true
}

/// Which date an album's "year" is read from: the track's original
/// first-release date, or the specific release/reissue it was sourced from.
/// Persisted; defaults to `Original` (the long-standing, expected behaviour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlbumYearSource {
    #[default]
    Original,
    Release,
}

/// A persisted window rectangle (outer position + content size) and maximized
/// state, so the app reopens where and how the user left it. Physical desktop
/// pixels, matching Rune's `WindowPlacement` / `WindowOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    #[serde(default)]
    pub maximized: bool,
}

/// Decide the geometry to persist from the current window placement plus whatever
/// was saved before. Pure so it can be unit-tested without a live window.
///
/// - Minimized: return `None`, a minimized rect would poison the saved "normal"
///   geometry, and there's nothing useful to restore to.
/// - Maximized: keep the previously-saved *normal* rect (un-maximize returns
///   there) and only flip the `maximized` flag; if nothing was saved yet, fall
///   back to the current rect.
/// - Normal: store the current rect with `maximized = false`.
fn geometry_to_save(
    pos: (i32, i32),
    size: (u32, u32),
    maximized: bool,
    minimized: bool,
    prev: Option<WindowGeometry>,
) -> Option<WindowGeometry> {
    if minimized {
        return None;
    }
    let current = WindowGeometry {
        x: pos.0,
        y: pos.1,
        w: size.0,
        h: size.1,
        maximized: false,
    };
    if maximized {
        Some(WindowGeometry {
            maximized: true,
            ..prev.unwrap_or(current)
        })
    } else {
        Some(current)
    }
}

/// A connected display's bounds in physical desktop pixels (origins may be
/// negative on multi-monitor layouts). `primary` marks the fallback display used
/// when a restored window overlaps none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayBounds {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub primary: bool,
}

/// Adjust a restored window rect so it stays visible on a currently-connected
/// display. Picks the display with the largest overlap and clamps the window
/// fully inside it; if the saved rect overlaps NONE (e.g. an external monitor was
/// unplugged, or the resolution shrank), it is centered on the primary display.
/// With no display info (enumeration failed / headless), the geometry is returned
/// unchanged, better to trust the saved position than to force-move the window.
fn clamp_to_displays(geom: WindowGeometry, displays: &[DisplayBounds]) -> WindowGeometry {
    if displays.is_empty() {
        return geom;
    }
    // The display the window mostly sits on; ties resolve to any max (all equal).
    let best = displays
        .iter()
        .max_by_key(|d| overlap_area(&geom, d))
        .expect("displays is non-empty");
    if overlap_area(&geom, best) > 0 {
        fit_into(geom, best)
    } else {
        // Overlaps nothing (unplugged monitor / resolution change) → recenter.
        let primary = displays.iter().find(|d| d.primary).unwrap_or(&displays[0]);
        center_on(geom, primary)
    }
}

/// Area (physical px²) the window rect and a display share; 0 if disjoint. Widened
/// to i64 so a 4K-scale rect can't overflow.
fn overlap_area(g: &WindowGeometry, d: &DisplayBounds) -> i64 {
    let (gx, gy) = (i64::from(g.x), i64::from(g.y));
    let (dx, dy) = (i64::from(d.x), i64::from(d.y));
    let ix = (gx + i64::from(g.w)).min(dx + i64::from(d.w)) - gx.max(dx);
    let iy = (gy + i64::from(g.h)).min(dy + i64::from(d.h)) - gy.max(dy);
    ix.max(0) * iy.max(0)
}

/// Shrink the window to the display if larger, then slide it fully inside.
fn fit_into(g: WindowGeometry, d: &DisplayBounds) -> WindowGeometry {
    let (w, h) = (g.w.min(d.w), g.h.min(d.h));
    WindowGeometry {
        x: g.x.clamp(d.x, d.x + (d.w - w) as i32),
        y: g.y.clamp(d.y, d.y + (d.h - h) as i32),
        w,
        h,
        maximized: g.maximized,
    }
}

/// Center the window on the display (shrinking to fit first).
fn center_on(g: WindowGeometry, d: &DisplayBounds) -> WindowGeometry {
    let (w, h) = (g.w.min(d.w), g.h.min(d.h));
    WindowGeometry {
        x: d.x + ((d.w - w) / 2) as i32,
        y: d.y + ((d.h - h) / 2) as i32,
        w,
        h,
        maximized: g.maximized,
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            remotes: Vec::new(),
            remotes_disabled: Vec::new(),
            theme: "system".into(),
            sidebar_width: DEFAULT_WIDTH,
            sidebar_collapsed: false,
            ui_zoom: 1.0,
            locale: "system".into(),
            nav_hidden: Vec::new(),
            sources_disabled: Vec::new(),
            managed_library_path: String::new(),
            managed_library_template: "artist-album".into(),
            managed_library_strategy: "copy".into(),
            eq_units: Vec::new(),
            eq_enabled: false,
            eq_preamp: 0.0,
            eq_bands: Vec::new(),
            indexing_mode: "auto".into(),
            loudness_analysis: "throttled".into(),
            profiles: Vec::new(),
            active_profile: 0,
            window: None,
            album_year_source: AlbumYearSource::Original,
            show_loudness_columns: true,
            update_auto_check: true,
            update_auto_install: false,
            move_prompt_kept: None,
        }
    }
}

/// Load the config: defaults if missing/garbled, migrating the pre-TOML
/// plain-text files (`sources.txt`/`theme.txt`/`sidebar.txt`) on first run.
fn load() -> Config {
    if let Some(p) = config_path()
        && let Ok(body) = std::fs::read_to_string(&p)
        && let Ok(cfg) = toml::from_str::<Config>(&body)
    {
        return cfg;
    }
    migrate_legacy().unwrap_or_default()
}

/// Write the config as pretty TOML (best-effort).
fn save(cfg: &Config) {
    if let Some(p) = config_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(body) = toml::to_string_pretty(cfg) {
            let _ = std::fs::write(&p, body);
        }
    }
}

/// Build a `Config` from the legacy plain-text files if any are present, and
/// persist it as `config.toml` so the migration happens exactly once.
fn migrate_legacy() -> Option<Config> {
    let dir = soul_player_dir()?;
    let mut cfg = Config::default();
    let mut found = false;
    if let Ok(s) = std::fs::read_to_string(dir.join("sources.txt")) {
        cfg.sources = s
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        found = true;
    }
    if let Ok(s) = std::fs::read_to_string(dir.join("theme.txt")) {
        cfg.theme = s.trim().to_string();
        found = true;
    }
    if let Ok(s) = std::fs::read_to_string(dir.join("sidebar.txt")) {
        let mut lines = s.lines();
        if let Some(w) = lines.next().and_then(|l| l.trim().parse::<f32>().ok()) {
            cfg.sidebar_width = w.clamp(MIN_WIDTH, MAX_WIDTH);
        }
        cfg.sidebar_collapsed = lines
            .next()
            .map(|l| l.trim() == "collapsed")
            .unwrap_or(false);
        found = true;
    }
    found.then(|| {
        save(&cfg);
        cfg
    })
}

// --- per-setting API (each save_* load-modify-writes the one file) ---

/// The saved watched folders (empty if none). Superseded by
/// [`load_sources_with_enabled`]; retained for back-compat (and the config tests).
#[allow(dead_code)]
pub fn load_sources() -> Vec<PathBuf> {
    load().sources.into_iter().map(PathBuf::from).collect()
}

/// Persist the watched folders. Only UTF-8-representable paths are saved, the
/// rare non-UTF-8 path is skipped rather than corrupting the file. This plain
/// source-list save carries no per-source enabled info, so it clears any stored
/// disabled subset (everything becomes enabled) rather than stranding a stale
/// flag; `save_sources_enabled` is the enabled-aware counterpart. Retained for
/// back-compat (and exercised by the config tests).
#[allow(dead_code)]
pub fn save_sources(folders: &[PathBuf]) {
    let mut cfg = load();
    cfg.sources = folders
        .iter()
        .filter_map(|p| p.to_str().map(String::from))
        .collect();
    cfg.sources_disabled.clear();
    save(&cfg);
}

/// The saved sources paired with their enabled flag. A path absent from
/// `sources_disabled` (which includes every path in an old config written before
/// the field existed) loads as enabled.
pub fn load_sources_with_enabled() -> Vec<(PathBuf, bool)> {
    let cfg = load();
    let disabled: std::collections::HashSet<&str> =
        cfg.sources_disabled.iter().map(String::as_str).collect();
    cfg.sources
        .iter()
        .map(|s| (PathBuf::from(s), !disabled.contains(s.as_str())))
        .collect()
}

/// Persist the sources together with their enabled flags: the full set goes to
/// `sources`, and the disabled subset to `sources_disabled`. Only UTF-8 paths are
/// saved (a non-UTF-8 path is skipped rather than corrupting the file).
pub fn save_sources_enabled(entries: &[(PathBuf, bool)]) {
    let mut cfg = load();
    cfg.sources = entries
        .iter()
        .filter_map(|(p, _)| p.to_str().map(String::from))
        .collect();
    cfg.sources_disabled = entries
        .iter()
        .filter(|(_, en)| !en)
        .filter_map(|(p, _)| p.to_str().map(String::from))
        .collect();
    save(&cfg);
}

/// The saved remote-source server URLs (empty if none), in the order they
/// were added. Reload these at startup and reconnect each one (silently,
/// via the stored keychain refresh token) so remote sources survive a
/// restart, see `soul_player_host::PlayerHandle::reconnect_remote_source`.
pub fn load_remotes() -> Vec<String> {
    load().remotes
}

/// Persist `url` as an added remote source (load-modify-write): appended
/// only if not already present, so re-adding (or a startup reconnect that
/// re-saves) dedupes rather than growing the list. The refresh token itself
/// is never written here, only the OS keychain holds it.
pub fn save_remote(url: &str) {
    let mut cfg = load();
    if !cfg.remotes.iter().any(|u| u == url) {
        cfg.remotes.push(url.to_string());
    }
    save(&cfg);
}

/// Drop `url` from the persisted remote sources (load-modify-write); a no-op
/// if it wasn't there. Paired with
/// [`PlayerHandle::remove_remote_source`](soul_player_host::PlayerHandle::remove_remote_source)
/// and deleting its OS keychain refresh token so a removed remote leaves
/// nothing behind, see the Settings → Sources Disconnect control
/// (`disconnect_remote_source`, Inc 6).
pub fn remove_remote(url: &str) {
    let mut cfg = load();
    cfg.remotes.retain(|u| u != url);
    // Drop any stored enabled/disabled flag too, so a later re-add of this
    // same url starts fresh (enabled) rather than inheriting a stale toggle,
    // mirrors `PlayerHandle::remove_remote_source` clearing its own
    // `Inner::remote_enabled` entry on the host side.
    cfg.remotes_disabled.retain(|u| u != url);
    save(&cfg);
}

/// Server URLs (a subset of `remotes`) the user has disabled, the remote
/// counterpart of [`load_sources_with_enabled`]'s disabled half, kept as its
/// own flat list (rather than paired with `remotes`) since `remotes` is
/// otherwise mutated one url at a time ([`save_remote`]/[`remove_remote`]).
/// Checked once at startup (`reconnect_remotes_on_startup`) to re-apply each
/// remote's enabled flag on the freshly reconnected host, a fresh launch's
/// `PlayerHandle` starts every remote enabled until told otherwise.
pub fn load_remotes_disabled() -> Vec<String> {
    load().remotes_disabled
}

/// Persist `url`'s enabled flag (load-modify-write), named to match
/// [`soul_player_host::PlayerHandle::set_remote_enabled`] on the host side:
/// added to the disabled set when `enabled` is `false`, dropped when `true`.
/// Independent of `remotes` (applies even to a url not yet in that list), so
/// toggling never races a concurrent add/remove of the url itself.
pub fn set_remote_enabled(url: &str, enabled: bool) {
    let mut cfg = load();
    if enabled {
        cfg.remotes_disabled.retain(|u| u != url);
    } else if !cfg.remotes_disabled.iter().any(|u| u == url) {
        cfg.remotes_disabled.push(url.to_string());
    }
    save(&cfg);
}

/// The saved theme preference ("system"/"light"/"dark").
pub fn load_theme_pref() -> Option<String> {
    Some(load().theme)
}

/// Persist the theme preference.
pub fn save_theme_pref(pref: &str) {
    let mut cfg = load();
    cfg.theme = pref.to_string();
    save(&cfg);
}

/// The saved album-year source (which date the UI shows/sorts by). Default Original.
pub fn load_album_year() -> AlbumYearSource {
    load().album_year_source
}

/// Persist the album-year source.
pub fn save_album_year(src: AlbumYearSource) {
    let mut cfg = load();
    cfg.album_year_source = src;
    save(&cfg);
}

/// Whether the track list shows the per-track loudness columns (DR + LUFS). Default `true`.
pub fn load_show_loudness_columns() -> bool {
    load().show_loudness_columns
}

/// Persist the loudness-columns visibility preference.
pub fn save_show_loudness_columns(show: bool) {
    let mut cfg = load();
    cfg.show_loudness_columns = show;
    save(&cfg);
}

/// Settings → Updates: (check automatically, install automatically on quit).
/// Defaults `(true, false)`.
pub fn load_update_prefs() -> (bool, bool) {
    let cfg = load();
    (cfg.update_auto_check, cfg.update_auto_install)
}

/// Persist the "check for updates automatically" switch.
pub fn save_update_auto_check(on: bool) {
    let mut cfg = load();
    cfg.update_auto_check = on;
    save(&cfg);
}

/// Persist the "install updates automatically" switch.
pub fn save_update_auto_install(on: bool) {
    let mut cfg = load();
    cfg.update_auto_install = on;
    save(&cfg);
}

/// macOS: the bundle path the user kept the app at (see `app_location`).
pub fn load_move_prompt_kept() -> Option<String> {
    load().move_prompt_kept
}

/// Remember "Keep Here" for this bundle path.
pub fn save_move_prompt_kept(bundle: &str) {
    let mut cfg = load();
    cfg.move_prompt_kept = Some(bundle.to_string());
    save(&cfg);
}

/// The persisted library indexing-speed token (`auto` | `max` | `gentle`);
/// defaults to `auto`. Map it through [`soul_player_host::IndexingMode::from_config`].
pub fn load_indexing_mode() -> String {
    load().indexing_mode
}

/// Persist the library indexing-speed token (from
/// [`soul_player_host::IndexingMode::as_config`]).
pub fn save_indexing_mode(mode: &str) {
    let mut cfg = load();
    cfg.indexing_mode = mode.to_string();
    save(&cfg);
}

/// The persisted background-loudness-analysis token (`off` | `throttled` | `full`);
/// defaults to `throttled`. Map it through
/// [`soul_player_host::LoudnessAnalysisMode::from_config`].
pub fn load_loudness_analysis() -> String {
    load().loudness_analysis
}

/// Persist the background-loudness-analysis token (from
/// [`soul_player_host::LoudnessAnalysisMode::as_config`]).
pub fn save_loudness_analysis(mode: &str) {
    let mut cfg = load();
    cfg.loudness_analysis = mode.to_string();
    save(&cfg);
}

/// The saved window geometry to restore on launch, or `None` on a fresh install.
pub fn load_window_geometry() -> Option<WindowGeometry> {
    load().window
}

/// Enumerate connected displays in physical-pixel desktop coordinates, for the
/// off-screen clamp. Stateless (`DisplayInfo::all()`: no winit event loop). Empty
/// on failure (the clamp then trusts the saved geometry) and in tests (hardware is
/// never touched; [`clamp_to_displays`] is unit-tested directly).
#[cfg(not(test))]
fn displays() -> Vec<DisplayBounds> {
    display_info::DisplayInfo::all()
        .unwrap_or_default()
        .into_iter()
        .map(|d| DisplayBounds {
            x: d.x,
            y: d.y,
            w: d.width,
            h: d.height,
            primary: d.is_primary,
        })
        .collect()
}

#[cfg(test)]
fn displays() -> Vec<DisplayBounds> {
    Vec::new()
}

/// The window geometry to restore on launch: the saved rect, clamped so it's
/// visible on a currently-connected display (recovering from an unplugged monitor
/// or a resolution change). `None` on a fresh install.
pub fn restore_window_geometry() -> Option<WindowGeometry> {
    Some(clamp_to_displays(load_window_geometry()?, &displays()))
}

/// Persist the window geometry derived from the current placement: pass the outer
/// position, content size, and maximized/minimized flags (e.g. from Rune's
/// `WindowPlacement`). A minimized window is skipped so it never overwrites the
/// stored normal rect.
pub fn save_window_geometry(pos: (i32, i32), size: (u32, u32), maximized: bool, minimized: bool) {
    let mut cfg = load();
    if let Some(g) = geometry_to_save(pos, size, maximized, minimized, cfg.window) {
        cfg.window = Some(g);
        save(&cfg);
    }
}

/// Restore the sidebar geometry `(width, collapsed)`; width is clamped to the
/// legal range (defensive against a hand-edited file).
pub fn load_sidebar() -> (f32, bool) {
    let cfg = load();
    (
        cfg.sidebar_width.clamp(MIN_WIDTH, MAX_WIDTH),
        cfg.sidebar_collapsed,
    )
}

/// Persist the sidebar geometry.
pub fn save_sidebar(width: f32, collapsed: bool) {
    let mut cfg = load();
    cfg.sidebar_width = width;
    cfg.sidebar_collapsed = collapsed;
    save(&cfg);
}

/// The persisted content-pane zoom (1.0 if unset). The caller sanitizes/clamps it.
pub fn load_ui_zoom() -> f32 {
    load().ui_zoom
}

/// Persist the content-pane zoom factor.
pub fn save_ui_zoom(zoom: f32) {
    let mut cfg = load();
    cfg.ui_zoom = zoom;
    save(&cfg);
}

/// The nav items the user has hidden (empty = all visible).
pub fn load_nav_hidden() -> Vec<String> {
    load().nav_hidden
}

/// Persist the hidden nav items.
pub fn save_nav_hidden(hidden: &[String]) {
    let mut cfg = load();
    cfg.nav_hidden = hidden.to_vec();
    save(&cfg);
}

/// The saved locale preference ("system"/"en"/"de"/"es"/"zh"/"cs").
pub fn load_locale() -> String {
    load().locale
}

/// Persist the locale preference.
pub fn save_locale(locale: &str) {
    let mut cfg = load();
    cfg.locale = locale.to_string();
    save(&cfg);
}

// --- managed-library (write-path organizer) preference ---

/// Extract the managed-library tuple `(destination, template-preset-id, strategy)`
/// from a `Config`, an empty stored path maps to `None` (unset). The read core of
/// [`load_managed_library`], split out so the roundtrip test can exercise it on an
/// isolated `Config` (never the shared per-process test config file).
fn managed_from(cfg: &Config) -> (Option<PathBuf>, String, String) {
    let dest = if cfg.managed_library_path.is_empty() {
        None
    } else {
        Some(PathBuf::from(&cfg.managed_library_path))
    };
    (
        dest,
        cfg.managed_library_template.clone(),
        cfg.managed_library_strategy.clone(),
    )
}

/// Apply a managed-library choice onto a `Config` (a `None` or non-UTF-8 dest
/// stores an empty path = unset, rather than corrupting the file). The write core
/// of [`save_managed_library`].
fn apply_managed(cfg: &mut Config, dest: Option<&Path>, template_id: &str, strategy: &str) {
    cfg.managed_library_path = dest
        .and_then(|p| p.to_str())
        .map(String::from)
        .unwrap_or_default();
    cfg.managed_library_template = template_id.to_string();
    cfg.managed_library_strategy = strategy.to_string();
}

/// The managed-library config: `(destination, template-preset-id, strategy)`. An
/// empty stored path is `None` (unset); the template id and strategy fall back to
/// `"artist-album"` / `"copy"` in a config written before the feature existed.
pub fn load_managed_library() -> (Option<PathBuf>, String, String) {
    managed_from(&load())
}

/// Persist the managed-library config. `None` (or a non-UTF-8 path) stores an
/// empty destination (= unset) rather than corrupting the file.
pub fn save_managed_library(dest: Option<&Path>, template_id: &str, strategy: &str) {
    let mut cfg = load();
    apply_managed(&mut cfg, dest, template_id, strategy);
    save(&cfg);
}

// --- output device preference ---
// (Kept as its own `output_device.txt` rather than folded into `config.toml`;
// a follow-up can migrate it, but the behaviour is unchanged.)

/// `<config>/soul-player/output_device.txt`: the persisted output-device choice.
/// A blank / missing file means "system default".
pub fn output_device_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("output_device.txt"))
}

/// The saved output-device name, or `None` for the system default (also `None` if
/// the file is missing / unreadable / blank).
pub fn load_output_device() -> Option<String> {
    output_device_path().and_then(|p| load_device_from(&p))
}

/// Persist the chosen output device (best-effort, a failed write never breaks the
/// UI). `None` (or a name that trims to empty) writes an empty file, meaning
/// "system default", the file is emptied rather than deleted so the choice stays
/// explicit (and doesn't error when it is already absent).
pub fn save_output_device(name: Option<&str>) {
    if let Some(p) = output_device_path() {
        let _ = save_device_to(&p, name);
    }
}

/// Read a single trimmed device name from `path` (the testable core); blank or
/// missing = `None` (the system default).
fn load_device_from(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Write `name` to `path`, creating the parent dir; `None` or a blank name writes
/// an empty file (= system default). The testable core of `save_output_device`.
fn save_device_to(path: &Path, name: Option<&str>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or("");
    std::fs::write(path, body)
}

// --- exclusive (bit-perfect) output preference ---
// A sibling of `output_device.txt`: both describe how the output device is opened.

/// `<config>/soul-player/output_exclusive.txt`: the persisted exclusive-output
/// (bit-perfect) choice. `"1"` = exclusive requested; blank / missing / anything
/// else = shared (the default).
pub fn output_exclusive_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("output_exclusive.txt"))
}

/// Whether exclusive output is requested (default `false` if missing / unreadable).
pub fn load_output_exclusive() -> bool {
    output_exclusive_path()
        .map(|p| load_exclusive_from(&p))
        .unwrap_or(false)
}

/// Persist the exclusive-output choice (best-effort, a failed write never breaks
/// the UI).
pub fn save_output_exclusive(on: bool) {
    if let Some(p) = output_exclusive_path() {
        let _ = save_exclusive_to(&p, on);
    }
}

/// Read the exclusive flag from `path` (the testable core): a trimmed `"1"` is on,
/// anything else / missing is off.
fn load_exclusive_from(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Write the exclusive flag to `path`, creating the parent dir. The testable core
/// of `save_output_exclusive`.
fn save_exclusive_to(path: &Path, on: bool) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, if on { "1" } else { "" })
}

/// `<config>/soul-player/output_exclusive_strict.txt`: the persisted **strict**
/// (exclusive-or-stop) choice. `"1"` = strict; blank / missing = off (fall back to
/// shared). Only meaningful together with `output_exclusive`.
pub fn output_exclusive_strict_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("output_exclusive_strict.txt"))
}

/// Whether strict exclusive is requested (default `false`). Reuses the same `"1"`
/// semantics as [`load_output_exclusive`].
pub fn load_output_exclusive_strict() -> bool {
    output_exclusive_strict_path()
        .map(|p| load_exclusive_from(&p))
        .unwrap_or(false)
}

/// Persist the strict-exclusive choice (best-effort).
pub fn save_output_exclusive_strict(on: bool) {
    if let Some(p) = output_exclusive_strict_path() {
        let _ = save_exclusive_to(&p, on);
    }
}

// --- ReplayGain preference ---

/// `<config>/soul-player/replaygain.txt`: the persisted ReplayGain choice, two
/// lines: the mode (`off`/`track`/`album`) then the preamp in dB (e.g.
/// `"track\n3"`). A missing / unreadable / partial file falls back to the
/// defaults (`Off`, `0.0`).
pub fn replaygain_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("replaygain.txt"))
}

/// The saved `(mode, preamp_db)`, defaulting to `(Off, 0.0)`.
pub fn load_replaygain() -> (ReplayGainMode, f32) {
    replaygain_path()
        .map(|p| load_rg_from(&p))
        .unwrap_or((ReplayGainMode::Off, 0.0))
}

/// Persist the ReplayGain choice (best-effort, a failed write never breaks the UI).
pub fn save_replaygain(mode: ReplayGainMode, preamp_db: f32) {
    if let Some(p) = replaygain_path() {
        let _ = save_rg_to(&p, mode, preamp_db);
    }
}

/// The saved DSP chain (ordered, typed DSP units), defaulting to a single off/flat
/// EQ unit. Unknown-kind entries (a config from a newer build) are skipped. Migrates
/// the pre-chain single-EQ fields into one EQ unit when `eq_units` is absent.
pub fn load_dsp_chain() -> Vec<DspUnit> {
    let cfg = load();
    if !cfg.eq_units.is_empty() {
        return cfg
            .eq_units
            .iter()
            .filter_map(DspUnitCfg::to_unit)
            .collect();
    }
    // Migrate a legacy single EQ, or fall back to one default (off) unit.
    let fallback = Band {
        kind: BandKind::Peaking,
        freq: 1000.0,
        gain_db: 0.0,
        q: 1.0,
        enabled: true,
    };
    let bands: Vec<Band> = if cfg.eq_bands.is_empty() {
        default_bands().to_vec()
    } else {
        cfg.eq_bands
            .iter()
            .take(MAX_BANDS)
            .map(|b| b.to_band(&fallback))
            .collect()
    };
    vec![DspUnit::Eq(EqUnit {
        bands,
        preamp_db: cfg.eq_preamp,
        enabled: cfg.eq_enabled,
        name: None,
    })]
}

/// Persist just the legacy `eq_units` chain (load-modify-write). Superseded in
/// production by [`save_profiles`] (which mirrors the active chain into `eq_units`);
/// kept as a test helper for the legacy-chain migration path that [`load_dsp_chain`]
/// still reads.
#[cfg(test)]
pub fn save_dsp_chain(units: &[DspUnit]) {
    let mut cfg = load();
    cfg.eq_units = units.iter().map(DspUnitCfg::from).collect();
    save(&cfg);
}

/// The saved audio profiles as a [`ProfileSet`]. Always has **at least one** profile:
/// a config written before profiles existed (no `profiles`, just `eq_units`) migrates
/// to a single `"Default"` profile from the existing chain. The active index is clamped.
pub fn load_profiles() -> ProfileSet {
    let cfg = load();
    if !cfg.profiles.is_empty() {
        return ProfileSet::new(
            cfg.profiles.iter().map(ProfileCfg::to_profile).collect(),
            cfg.active_profile,
        );
    }
    // Migrate: one "Default" profile from the legacy chain (handles `eq_units` + the
    // pre-chain single-EQ fields, and falls back to one default unit).
    ProfileSet::single(Profile::new("Default", DspChain::new(load_dsp_chain())))
}

/// Serialize a profile to the portable JSON used by exported `.json` files (and read by
/// the future DAP): `{ "name": ..., "chain": [ <tagged DspUnitCfg> ] }`.
pub fn profile_to_json(p: &Profile) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&ProfileCfg::from_profile(p))
}

/// Parse a profile from the portable JSON. Unknown-kind units are skipped (so a file
/// from a newer build still imports what this build understands).
pub fn profile_from_json(text: &str) -> Result<Profile, serde_json::Error> {
    Ok(serde_json::from_str::<ProfileCfg>(text)?.to_profile())
}

/// Persist the audio profiles + active index. Also mirrors the active profile's chain
/// into the legacy `eq_units` key, so a rollback to a pre-profiles build still loads
/// the right live chain.
pub fn save_profiles(set: &ProfileSet) {
    let mut cfg = load();
    cfg.profiles = set
        .profiles()
        .iter()
        .map(ProfileCfg::from_profile)
        .collect();
    cfg.active_profile = set.active_index();
    cfg.eq_units = set
        .active_chain()
        .map(|c| c.units.iter().map(DspUnitCfg::from).collect())
        .unwrap_or_default();
    save(&cfg);
}

/// Map a persisted mode string to the enum; anything unknown is `Off`.
fn mode_from_str(s: &str) -> ReplayGainMode {
    match s.trim() {
        "track" => ReplayGainMode::Track,
        "album" => ReplayGainMode::Album,
        _ => ReplayGainMode::Off,
    }
}

/// The on-disk token for a mode (the inverse of [`mode_from_str`]).
fn mode_as_str(mode: ReplayGainMode) -> &'static str {
    match mode {
        ReplayGainMode::Off => "off",
        ReplayGainMode::Track => "track",
        ReplayGainMode::Album => "album",
    }
}

/// Read `(mode, preamp)` from `path` (the testable core): line 1 = mode, line 2 =
/// preamp dB. A missing/unparseable line falls back to that field's default.
fn load_rg_from(path: &Path) -> (ReplayGainMode, f32) {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let mut lines = s.lines();
            let mode = lines
                .next()
                .map(mode_from_str)
                .unwrap_or(ReplayGainMode::Off);
            let preamp = lines
                .next()
                .and_then(|l| l.trim().parse::<f32>().ok())
                .unwrap_or(0.0);
            (mode, preamp)
        }
        Err(_) => (ReplayGainMode::Off, 0.0),
    }
}

/// Write `(mode, preamp)` to `path`, creating the parent dir. The testable core
/// of [`save_replaygain`].
fn save_rg_to(path: &Path, mode: ReplayGainMode, preamp_db: f32) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{}\n{preamp_db}", mode_as_str(mode)))
}

// --- hybrid-ReplayGain loudness cache ---

/// `<config>/soul-player/loudness_cache.txt`: the persisted loudness & dynamic-
/// range metrics for tracks the backend background-analyzed. One line per entry:
/// `"<integrated>\t<lra>\t<true_peak>\t<plr>\t<dr>\t<mtime_secs>\t<size>\t<path>"`.
/// The mtime+size are a cheap freshness stamp: an entry whose file no longer
/// matches (edited / replaced / moved) is dropped on load. The 8-field layout also
/// makes an older 4-field (gain-only) cache parse as empty, migration-free.
pub fn loudness_cache_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("loudness_cache.txt"))
}

/// The saved, still-valid `(path, metrics)` entries (empty if the file is missing
/// / unreadable). Each is validated against the file's CURRENT mtime+size; stale
/// or vanished files are skipped.
pub fn load_loudness_cache() -> Vec<(PathBuf, TrackLoudness)> {
    loudness_cache_path()
        .map(|p| load_loudness_from(&p))
        .unwrap_or_default()
}

/// Record a freshly-computed metric set for `audio_path` (best-effort, a failed
/// write never breaks the UI). Upserts by path (the new stamp wins) and rewrites
/// the file, so the cache dedups rather than growing without bound.
pub fn record_loudness(audio_path: &Path, loudness: &TrackLoudness) {
    if let Some(p) = loudness_cache_path() {
        let _ = record_loudness_to(&p, audio_path, loudness);
    }
}

/// A file's `(mtime_secs, size)` freshness stamp, or `None` if it can't be stat'd.
fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((mtime, m.len()))
}

/// Read + validate the cache from `path` (the testable core). A line parses as the
/// 8 tab-separated fields above (the path, the last field, may itself contain
/// tabs), and is kept only when the file still matches the recorded mtime+size.
fn load_loudness_from(path: &Path) -> Vec<(PathBuf, TrackLoudness)> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in body.lines() {
        if line.is_empty() {
            continue;
        }
        let mut it = line.splitn(8, '\t');
        let (Some(int), Some(lra), Some(tp), Some(plr), Some(dr), Some(mtime), Some(size), Some(p)) = (
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
            it.next(),
        ) else {
            continue;
        };
        let (Ok(int), Ok(lra), Ok(tp), Ok(plr), Ok(dr), Ok(mtime), Ok(size)) = (
            int.parse::<f32>(),
            lra.parse::<f32>(),
            tp.parse::<f32>(),
            plr.parse::<f32>(),
            dr.parse::<f32>(),
            mtime.parse::<u64>(),
            size.parse::<u64>(),
        ) else {
            continue;
        };
        let audio = PathBuf::from(p);
        if file_stamp(&audio) == Some((mtime, size)) {
            out.push((
                audio,
                TrackLoudness {
                    integrated_lufs: int,
                    lra,
                    true_peak_dbtp: tp,
                    plr,
                    dr,
                },
            ));
        }
    }
    out
}

/// Upsert `(audio_path, metrics)` into the cache at `cache_path` and rewrite it
/// (the testable core of [`record_loudness`]). Skips a non-UTF-8 or un-stat-able
/// audio path (nothing to validate against later). Existing entries are reloaded
/// WITHOUT revalidation so a momentarily-offline file isn't purged by a write for
/// a different track.
fn record_loudness_to(
    cache_path: &Path,
    audio_path: &Path,
    loudness: &TrackLoudness,
) -> std::io::Result<()> {
    let Some((mtime, size)) = file_stamp(audio_path) else {
        return Ok(()); // can't stamp it → nothing worth persisting
    };
    let Some(path_str) = audio_path.to_str() else {
        return Ok(()); // non-UTF-8 path → skip rather than corrupt the file
    };
    if let Some(dir) = cache_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Reload existing lines verbatim (key = the last/path field) so the upsert
    // dedups; old 4-field lines have no 8th field and are dropped on rewrite.
    let mut lines: Vec<(String, String)> = Vec::new(); // (path_key, full_line)
    if let Ok(body) = std::fs::read_to_string(cache_path) {
        for line in body.lines() {
            if line.is_empty() {
                continue;
            }
            if let Some(key) = line.splitn(8, '\t').nth(7) {
                lines.push((key.to_string(), line.to_string()));
            }
        }
    }
    let TrackLoudness {
        integrated_lufs,
        lra,
        true_peak_dbtp,
        plr,
        dr,
    } = *loudness;
    let fresh = format!(
        "{integrated_lufs}\t{lra}\t{true_peak_dbtp}\t{plr}\t{dr}\t{mtime}\t{size}\t{path_str}"
    );
    let mut replaced = false;
    for entry in &mut lines {
        if entry.0 == path_str {
            entry.1 = fresh.clone();
            replaced = true;
        }
    }
    if !replaced {
        lines.push((path_str.to_string(), fresh));
    }
    let body = lines
        .iter()
        .map(|(_, l)| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(cache_path, body)
}

// --- per-source scan cache (warm startup) ---

/// The on-disk scan cache: the host's per-source [`ScanCache`] map plus the
/// importer's [`SCAN_CACHE_VERSION`] it was written with. A load whose stored
/// version doesn't match the running importer discards the whole cache (a cold
/// re-scan) so no stale, differently-extracted field is ever reused.
#[derive(Serialize, Deserialize)]
struct PersistedCaches {
    version: u32,
    caches: HashMap<PathBuf, ScanCache>,
}

/// `<config>/soul-player/scan_caches.bin`: the per-source scan caches, stored in
/// postcard (binary): the map is path-keyed and the stamps are `u128`, neither of
/// which TOML carries cleanly, and the cache is large enough that a compact format
/// matters.
pub fn scan_caches_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("scan_caches.bin"))
}

/// The persisted per-source scan caches (empty if the file is missing / unreadable
/// / corrupt / written by an incompatible importer version). Seed the host with
/// this at startup, **before** `set_sources`, so the first scan is a warm diff.
pub fn load_scan_caches() -> HashMap<PathBuf, ScanCache> {
    scan_caches_path()
        .map(|p| load_scan_caches_from(&p))
        .unwrap_or_default()
}

/// Persist the host's current per-source scan caches (best-effort, a failed write
/// never breaks the UI). Call after a scan so the next launch is warm.
pub fn save_scan_caches(caches: &HashMap<PathBuf, ScanCache>) {
    if let Some(p) = scan_caches_path() {
        let _ = save_scan_caches_to(&p, caches);
    }
}

/// Read + version-check the cache from `path` (the testable core). A missing /
/// unreadable / malformed file, or one whose stored version != the running
/// [`SCAN_CACHE_VERSION`], yields an empty map (a cold start).
fn load_scan_caches_from(path: &Path) -> HashMap<PathBuf, ScanCache> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    match postcard::from_bytes::<PersistedCaches>(&bytes) {
        Ok(p) if p.version == SCAN_CACHE_VERSION => p.caches,
        _ => HashMap::new(),
    }
}

/// Serialize `caches` (stamped with the current [`SCAN_CACHE_VERSION`]) to `path`
/// in postcard (the testable core of [`save_scan_caches`]), creating the parent
/// dir as needed.
fn save_scan_caches_to(path: &Path, caches: &HashMap<PathBuf, ScanCache>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let payload = PersistedCaches {
        version: SCAN_CACHE_VERSION,
        // Clone is unavoidable here (we borrow the host's live map); the cache is
        // written once per scan, so the cost is immaterial.
        caches: caches.clone(),
    };
    let bytes = postcard::to_allocvec(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_toml_roundtrips() {
        let cfg = Config {
            sources: vec!["/music/a".into(), "/music/b".into()],
            remotes: vec!["http://server-a".into(), "http://server-b".into()],
            remotes_disabled: vec!["http://server-b".into()],
            theme: "dark".into(),
            sidebar_width: 300.0,
            sidebar_collapsed: true,
            ui_zoom: 1.3,
            locale: "de".into(),
            nav_hidden: vec!["home".into()],
            sources_disabled: Vec::new(),
            managed_library_path: "/music/managed".into(),
            managed_library_template: "flat".into(),
            managed_library_strategy: "move".into(),
            eq_units: vec![DspUnitCfg {
                kind: "eq".into(),
                name: Some("my-name".into()),
                enabled: true,
                preamp: -2.0,
                bands: vec![BandCfg {
                    kind: "low_shelf".into(),
                    freq: 90.0,
                    gain: 4.5,
                    q: 0.7,
                    enabled: true,
                }],
            }],
            eq_enabled: true,
            eq_preamp: -3.0,
            eq_bands: vec![BandCfg {
                kind: "low_shelf".into(),
                freq: 90.0,
                gain: 4.5,
                q: 0.7,
                enabled: true,
            }],
            indexing_mode: "gentle".into(),
            loudness_analysis: "full".into(),
            profiles: vec![ProfileCfg {
                name: "Studio".into(),
                chain: vec![DspUnitCfg {
                    kind: "eq".into(),
                    name: None,
                    enabled: true,
                    preamp: 1.0,
                    bands: Vec::new(),
                }],
            }],
            active_profile: 0,
            window: Some(WindowGeometry {
                x: 100,
                y: 80,
                w: 1280,
                h: 800,
                maximized: true,
            }),
            album_year_source: AlbumYearSource::Release,
            show_loudness_columns: false,
            update_auto_check: false,
            update_auto_install: true,
            move_prompt_kept: Some("/Users/me/Downloads/Soul Player.app".into()),
        };
        let body = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&body).unwrap();
        assert_eq!(back.sources, cfg.sources);
        assert_eq!(back.remotes, cfg.remotes);
        assert_eq!(back.remotes_disabled, cfg.remotes_disabled);
        assert_eq!(back.theme, "dark");
        assert_eq!(back.window, cfg.window);
        assert_eq!(back.sidebar_width, 300.0);
        assert!(back.sidebar_collapsed);
        assert_eq!(back.ui_zoom, 1.3);
        assert_eq!(back.locale, "de");
        assert!(back.eq_enabled);
        assert_eq!(back.eq_preamp, -3.0);
        assert_eq!(back.eq_bands, cfg.eq_bands);
        assert_eq!(back.eq_units, cfg.eq_units);
        assert_eq!(back.nav_hidden, vec!["home".to_string()]);
        assert_eq!(back.managed_library_path, "/music/managed");
        assert_eq!(back.managed_library_template, "flat");
        assert_eq!(back.managed_library_strategy, "move");
        assert_eq!(back.profiles, cfg.profiles);
        assert_eq!(back.active_profile, 0);
        assert_eq!(back.album_year_source, AlbumYearSource::Release);
        assert!(!back.show_loudness_columns);
        assert!(!back.update_auto_check && back.update_auto_install);
    }

    #[test]
    fn album_year_source_round_trips_and_defaults_to_original() {
        let _g = use_test_config_dir();
        assert_eq!(load_album_year(), AlbumYearSource::Original); // default
        save_album_year(AlbumYearSource::Release);
        assert_eq!(load_album_year(), AlbumYearSource::Release);
    }

    #[test]
    fn loudness_columns_round_trip_and_default_to_shown() {
        let _g = use_test_config_dir();
        assert!(load_show_loudness_columns(), "shown by default"); // default true
        save_show_loudness_columns(false);
        assert!(!load_show_loudness_columns());
        save_show_loudness_columns(true);
        assert!(load_show_loudness_columns());
    }

    #[test]
    fn update_prefs_round_trip_and_default_to_check_but_not_install() {
        let _g = use_test_config_dir();
        assert_eq!(
            load_update_prefs(),
            (true, false),
            "check on, auto-install off by default"
        );
        save_update_auto_check(false);
        save_update_auto_install(true);
        assert_eq!(load_update_prefs(), (false, true));
        save_update_auto_check(true);
        assert_eq!(load_update_prefs(), (true, true));
    }

    #[test]
    fn a_config_written_before_the_update_settings_keeps_checking() {
        // The e2e harness and older builds write configs without these keys: they
        // must load as "check on, install off", and a bare key sets just itself.
        let cfg: Config = toml::from_str("theme = \"dark\"\n").unwrap();
        assert!(cfg.update_auto_check && !cfg.update_auto_install);
        let cfg: Config = toml::from_str("update_auto_check = false\n").unwrap();
        assert!(!cfg.update_auto_check && !cfg.update_auto_install);
    }

    #[test]
    fn a_config_written_before_the_loudness_columns_setting_loads_as_shown() {
        // A TOML body with no `show_loudness_columns` key must deserialize to `true`
        // (the serde field default), not bool's `false`, existing users keep the
        // columns they have always had.
        let back: Config = toml::from_str("theme = 'dark'\n").unwrap();
        assert!(back.show_loudness_columns);
    }

    #[test]
    fn profiles_roundtrip_migrate_default_and_mirror_the_active_chain() {
        let _g = use_test_config_dir();
        // No profiles saved yet, but a legacy chain exists → one "Default" profile
        // carrying that chain.
        save_dsp_chain(&[DspUnit::Eq(EqUnit {
            preamp_db: -4.0,
            ..EqUnit::default()
        })]);
        let migrated = load_profiles();
        assert_eq!(migrated.active_index(), 0);
        assert_eq!(migrated.len(), 1, "a lone Default profile is synthesized");
        assert_eq!(migrated.active().unwrap().name, "Default");
        assert_eq!(
            migrated.active_chain().unwrap().units[0]
                .as_eq()
                .unwrap()
                .preamp_db,
            -4.0
        );

        // Two explicit profiles with active = 1 reload with their names, chains and
        // the active index intact.
        let studio = Profile::new(
            "Studio",
            DspChain::new(vec![DspUnit::Eq(EqUnit {
                preamp_db: -2.0,
                ..EqUnit::default()
            })]),
        );
        let bass = Profile::new(
            "Bass",
            DspChain::new(vec![DspUnit::Eq(EqUnit {
                preamp_db: 3.0,
                ..EqUnit::default()
            })]),
        );
        save_profiles(&ProfileSet::new(vec![studio.clone(), bass.clone()], 1));
        let back = load_profiles();
        assert_eq!(back.active_index(), 1);
        assert_eq!(back.len(), 2);
        assert_eq!(back.profiles()[0], studio);
        assert_eq!(back.profiles()[1], bass);

        // The active profile's chain mirrors into the legacy `eq_units`, so an older
        // build (load_dsp_chain) still loads the right live chain.
        assert_eq!(
            load_dsp_chain()[0].as_eq().unwrap().preamp_db,
            3.0,
            "eq_units mirrors the active profile"
        );

        // An out-of-range active index clamps to the last profile.
        save_profiles(&ProfileSet::new(vec![studio.clone()], 9));
        assert_eq!(
            load_profiles().active_index(),
            0,
            "the active index clamps into range"
        );
    }

    #[test]
    fn dsp_chain_roundtrips_and_an_untagged_entry_loads_as_eq() {
        let _g = use_test_config_dir();
        // A DspUnit::Eq survives the full save → load round-trip through the shared
        // config file (bands, preamp and enable all reload).
        let unit = DspUnit::Eq(EqUnit {
            bands: vec![Band {
                kind: BandKind::LowShelf,
                freq: 90.0,
                gain_db: 4.5,
                q: 0.7,
                enabled: true,
            }],
            preamp_db: -2.0,
            enabled: true,
            name: Some("my-name".into()),
        });
        save_dsp_chain(std::slice::from_ref(&unit));
        let back = load_dsp_chain();
        assert_eq!(back.len(), 1, "the saved unit reloads");
        assert_eq!(
            back[0].name(),
            Some("my-name"),
            "the user-given name reloads"
        );
        let eq = back[0].as_eq().expect("round-trips as an EQ unit");
        assert!(eq.enabled);
        assert_eq!(eq.preamp_db, -2.0);
        assert_eq!(eq.bands.len(), 1);
        assert_eq!(eq.bands[0].kind, BandKind::LowShelf);

        // An unnamed unit stays unnamed (the field is skipped on disk, defaulted on
        // load) rather than reloading as an empty string.
        let unnamed = DspUnit::Eq(EqUnit::default());
        save_dsp_chain(std::slice::from_ref(&unnamed));
        assert_eq!(
            load_dsp_chain()[0].name(),
            None,
            "an unnamed unit reloads as unnamed"
        );

        // An OLD entry written before the `kind` tag existed (no `kind` field) still
        // deserializes, defaulting to kind "eq", and maps to a DspUnit::Eq.
        let untagged: DspUnitCfg =
            toml::from_str("enabled = true\npreamp = -1.5\nbands = []\n").unwrap();
        assert_eq!(
            untagged.kind, "eq",
            "an untagged entry defaults to the EQ kind"
        );
        let as_unit = untagged
            .to_unit()
            .expect("an untagged (eq) entry builds a unit");
        assert!(matches!(as_unit, DspUnit::Eq(_)), "untagged → DspUnit::Eq");

        // An unknown future kind is skipped (forward-compatible), not misread as EQ.
        let unknown = DspUnitCfg {
            kind: "compressor".into(),
            name: None,
            enabled: true,
            preamp: 0.0,
            bands: Vec::new(),
        };
        assert!(unknown.to_unit().is_none(), "an unknown kind is skipped");
    }

    #[test]
    fn managed_library_roundtrips_and_empty_path_is_unset() {
        // Round-trip the real write (`apply_managed`) + read (`managed_from`) cores
        // through serde on an ISOLATED temp file, like the output-device /
        // replaygain tests, so this never contends with the many other tests that
        // write the shared per-process test config.toml (a concurrent lost-update
        // would otherwise reset these fields to default).
        let dir = std::env::temp_dir().join(format!("soul_rune_managed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let roundtrip = |dest: Option<&Path>, template: &str, strategy: &str| {
            let mut cfg = Config::default();
            apply_managed(&mut cfg, dest, template, strategy);
            std::fs::write(&path, toml::to_string_pretty(&cfg).unwrap()).unwrap();
            let back: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            managed_from(&back)
        };

        let (dest, template, strategy) = roundtrip(
            Some(Path::new("/music/managed")),
            "genre-artist-album",
            "move",
        );
        assert_eq!(dest, Some(PathBuf::from("/music/managed")));
        assert_eq!(template, "genre-artist-album");
        assert_eq!(strategy, "move");

        // A None destination stores an empty path (= unset) and reloads as None.
        let (dest, template, strategy) = roundtrip(None, "flat", "copy");
        assert_eq!(dest, None, "a None destination reloads as unset");
        assert_eq!(template, "flat");
        assert_eq!(strategy, "copy");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sources_disabled_roundtrips_and_old_config_is_all_enabled() {
        let _config_guard = use_test_config_dir();
        save_sources_enabled(&[(PathBuf::from("/a"), true), (PathBuf::from("/b"), false)]);
        let back = load_sources_with_enabled();
        assert_eq!(
            back,
            vec![(PathBuf::from("/a"), true), (PathBuf::from("/b"), false)]
        );
        // A legacy config (only `sources`, no `sources_disabled`) loads all-enabled.
        save_sources(&[PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(
            load_sources_with_enabled(),
            vec![(PathBuf::from("/a"), true), (PathBuf::from("/b"), true)]
        );
    }

    #[test]
    fn remotes_roundtrip_dedupe_and_remove() {
        let _config_guard = use_test_config_dir();
        // Drain to a known-empty starting point: `remotes` is additive
        // (save_remote appends, never overwrites), so, unlike the other
        // settings here, which fully replace their field, a sibling test
        // sharing this process-wide config file could otherwise leave a
        // stray url behind that would break the exact-equality assertions
        // below.
        for url in load_remotes() {
            remove_remote(&url);
        }
        save_remote("http://a");
        save_remote("http://b");
        assert_eq!(
            load_remotes(),
            vec!["http://a".to_string(), "http://b".to_string()]
        );

        // Re-saving an already-present url dedupes rather than duplicating.
        save_remote("http://a");
        assert_eq!(
            load_remotes(),
            vec!["http://a".to_string(), "http://b".to_string()],
            "re-adding the same url must not duplicate it"
        );

        remove_remote("http://a");
        assert_eq!(
            load_remotes(),
            vec!["http://b".to_string()],
            "removing a url drops only that one"
        );

        // Removing something never added is a no-op, not an error.
        remove_remote("http://never-added");
        assert_eq!(load_remotes(), vec!["http://b".to_string()]);
    }

    #[test]
    fn remotes_disabled_roundtrips_and_old_config_is_all_enabled() {
        let _config_guard = use_test_config_dir();
        for url in load_remotes() {
            remove_remote(&url);
        }
        save_remote("http://a");
        save_remote("http://b");

        // Fresh config, nothing toggled yet: everything enabled.
        assert!(load_remotes_disabled().is_empty());

        set_remote_enabled("http://a", false);
        assert_eq!(
            load_remotes_disabled(),
            vec!["http://a".to_string()],
            "disabling a remote adds it to the disabled set"
        );

        // Re-disabling dedupes rather than duplicating.
        set_remote_enabled("http://a", false);
        assert_eq!(load_remotes_disabled(), vec!["http://a".to_string()]);

        set_remote_enabled("http://a", true);
        assert!(
            load_remotes_disabled().is_empty(),
            "re-enabling drops it from the disabled set"
        );

        // Removing a remote drops any stored disabled flag for it too.
        set_remote_enabled("http://b", false);
        remove_remote("http://b");
        assert!(
            load_remotes_disabled().is_empty(),
            "removing a remote clears its disabled flag"
        );

        for url in load_remotes() {
            remove_remote(&url);
        }
    }

    #[test]
    fn partial_config_fills_defaults() {
        // A config written by an older build (no `locale` yet) still loads, with
        // the missing field taking its default.
        let back: Config = toml::from_str("theme = \"light\"\n").unwrap();
        assert_eq!(back.theme, "light");
        assert_eq!(back.locale, "system");
        assert_eq!(back.sidebar_width, DEFAULT_WIDTH);
    }

    /// `load()` migrates the pre-TOML plain-text files (`sources.txt`/`theme.txt`/
    /// `sidebar.txt`) into `config.toml` exactly once. A regression here silently
    /// wipes an upgrading user's saved sources, theme, and sidebar on first launch,
    /// so it is worth a direct `load()` test (not just `toml::from_str`).
    #[test]
    fn load_migrates_legacy_txt_files_once() {
        let _g = use_test_config_dir();
        let dir = soul_player_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        // Start clean (the test config dir is shared per-process), then drop in the
        // plain-text files an older build left behind, and NO config.toml.
        let _ = std::fs::remove_file(dir.join("config.toml"));
        std::fs::write(dir.join("sources.txt"), "/music/a\n/music/b\n").unwrap();
        std::fs::write(dir.join("theme.txt"), "dark\n").unwrap();
        std::fs::write(dir.join("sidebar.txt"), "300\ncollapsed\n").unwrap();

        let cfg = load();
        assert_eq!(
            cfg.sources,
            vec!["/music/a".to_string(), "/music/b".to_string()],
            "legacy sources migrate"
        );
        assert_eq!(cfg.theme, "dark", "legacy theme migrates");
        assert_eq!(cfg.sidebar_width, 300.0, "legacy sidebar width migrates");
        assert!(cfg.sidebar_collapsed, "legacy collapsed state migrates");
        assert!(
            dir.join("config.toml").exists(),
            "migration persists config.toml so it runs exactly once"
        );

        // With config.toml now written, a second load reads IT, not the legacy files:
        // delete them and prove the migrated values survive.
        for f in ["sources.txt", "theme.txt", "sidebar.txt"] {
            std::fs::remove_file(dir.join(f)).unwrap();
        }
        let again = load();
        assert_eq!(
            again.sources, cfg.sources,
            "second load reads the migrated config.toml, not the (now gone) legacy files"
        );
        assert_eq!(again.theme, "dark");

        let _ = std::fs::remove_file(dir.join("config.toml"));
    }

    /// A corrupt `config.toml` must recover to defaults, never panic or hang, the
    /// `toml::from_str` failure in `load()` falls through to the legacy migration
    /// (absent here) and then `Config::default()`.
    #[test]
    fn load_recovers_from_a_garbled_config() {
        let _g = use_test_config_dir();
        let dir = soul_player_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["sources.txt", "theme.txt", "sidebar.txt"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        std::fs::write(dir.join("config.toml"), "not = valid = toml [[[\n").unwrap();

        let cfg = load();
        assert_eq!(
            cfg.theme,
            Config::default().theme,
            "a garbled config recovers to the default theme, not a panic"
        );
        assert_eq!(
            cfg.sidebar_width, DEFAULT_WIDTH,
            "garbled config recovers to the default sidebar width"
        );
        assert!(
            cfg.sources.is_empty(),
            "garbled config recovers to no sources"
        );

        let _ = std::fs::remove_file(dir.join("config.toml"));
    }

    #[test]
    fn out_of_range_width_clamps() {
        let wide = Config {
            sidebar_width: 999.0,
            ..Config::default()
        };
        assert_eq!(wide.sidebar_width.clamp(MIN_WIDTH, MAX_WIDTH), MAX_WIDTH);
    }

    #[test]
    fn geometry_normal_window_stores_current_rect() {
        let g = geometry_to_save((50, 60), (1000, 700), false, false, None);
        assert_eq!(
            g,
            Some(WindowGeometry {
                x: 50,
                y: 60,
                w: 1000,
                h: 700,
                maximized: false
            })
        );
    }

    #[test]
    fn geometry_minimized_is_not_saved() {
        let prev = Some(WindowGeometry {
            x: 1,
            y: 2,
            w: 800,
            h: 600,
            maximized: false,
        });
        assert_eq!(geometry_to_save((0, 0), (0, 0), false, true, prev), None);
    }

    #[test]
    fn geometry_maximized_keeps_previous_normal_rect() {
        // When quitting maximized, the stored rect must stay the last *normal* one
        // so un-maximize returns there, only the flag flips.
        let prev = Some(WindowGeometry {
            x: 50,
            y: 60,
            w: 1000,
            h: 700,
            maximized: false,
        });
        let g = geometry_to_save((0, 0), (3840, 2160), true, false, prev);
        assert_eq!(
            g,
            Some(WindowGeometry {
                x: 50,
                y: 60,
                w: 1000,
                h: 700,
                maximized: true
            })
        );
    }

    #[test]
    fn geometry_maximized_with_no_history_falls_back_to_current() {
        let g = geometry_to_save((0, 0), (3840, 2160), true, false, None);
        assert_eq!(
            g,
            Some(WindowGeometry {
                x: 0,
                y: 0,
                w: 3840,
                h: 2160,
                maximized: true
            })
        );
    }

    // A primary 1920x1080 at origin and a secondary 2560x1440 to its right.
    fn two_monitors() -> Vec<DisplayBounds> {
        vec![
            DisplayBounds {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
                primary: true,
            },
            DisplayBounds {
                x: 1920,
                y: 0,
                w: 2560,
                h: 1440,
                primary: false,
            },
        ]
    }

    #[test]
    fn clamp_window_fully_on_a_monitor_is_unchanged() {
        let g = WindowGeometry {
            x: 100,
            y: 100,
            w: 800,
            h: 600,
            maximized: false,
        };
        assert_eq!(clamp_to_displays(g, &two_monitors()), g);
    }

    #[test]
    fn clamp_window_on_the_secondary_monitor_stays_there() {
        // Fully inside the secondary (1920..4480, 0..1440) → untouched.
        let g = WindowGeometry {
            x: 2000,
            y: 100,
            w: 1000,
            h: 800,
            maximized: false,
        };
        assert_eq!(clamp_to_displays(g, &two_monitors()), g);
    }

    #[test]
    fn clamp_window_hanging_off_an_edge_is_pulled_fully_onto_its_monitor() {
        // Mostly on the primary but spilling past its right edge (1600+800 > 1920).
        let g = WindowGeometry {
            x: 1600,
            y: 100,
            w: 800,
            h: 600,
            maximized: false,
        };
        let only_primary = &two_monitors()[..1];
        assert_eq!(
            clamp_to_displays(g, only_primary),
            WindowGeometry {
                x: 1120,
                y: 100,
                w: 800,
                h: 600,
                maximized: false
            }
        );
    }

    #[test]
    fn clamp_window_on_a_disconnected_monitor_centers_on_primary() {
        // Saved far away (an unplugged external) → overlaps nothing → centered on
        // the primary: x = (1920-800)/2 = 560, y = (1080-600)/2 = 240.
        let g = WindowGeometry {
            x: 5000,
            y: 5000,
            w: 800,
            h: 600,
            maximized: false,
        };
        assert_eq!(
            clamp_to_displays(g, &two_monitors()),
            WindowGeometry {
                x: 560,
                y: 240,
                w: 800,
                h: 600,
                maximized: false
            }
        );
    }

    #[test]
    fn clamp_window_larger_than_its_monitor_shrinks_to_fit() {
        let g = WindowGeometry {
            x: 0,
            y: 0,
            w: 3000,
            h: 2000,
            maximized: false,
        };
        let only_primary = &two_monitors()[..1];
        assert_eq!(
            clamp_to_displays(g, only_primary),
            WindowGeometry {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
                maximized: false
            }
        );
    }

    #[test]
    fn clamp_with_no_display_info_trusts_the_saved_geometry() {
        let g = WindowGeometry {
            x: -4000,
            y: -4000,
            w: 800,
            h: 600,
            maximized: false,
        };
        assert_eq!(clamp_to_displays(g, &[]), g);
    }

    #[test]
    fn clamp_preserves_the_maximized_flag() {
        let g = WindowGeometry {
            x: 5000,
            y: 5000,
            w: 800,
            h: 600,
            maximized: true,
        };
        assert!(clamp_to_displays(g, &two_monitors()).maximized);
    }

    #[test]
    fn save_then_load_output_device_roundtrips() {
        let dir = std::env::temp_dir().join(format!("soul_rune_dev_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("output_device.txt");
        save_device_to(&path, Some("Speakers (Realtek)")).unwrap();
        assert_eq!(
            load_device_from(&path),
            Some("Speakers (Realtek)".to_string()),
            "the saved device name reloads verbatim"
        );
        // None (and a blank name) persist as the system default, a blank file.
        save_device_to(&path, None).unwrap();
        assert_eq!(
            load_device_from(&path),
            None,
            "None reloads as the system default"
        );
        save_device_to(&path, Some("   ")).unwrap();
        assert_eq!(
            load_device_from(&path),
            None,
            "a blank name is the system default, not a device"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_output_device_is_none() {
        assert!(load_device_from(Path::new("/no/such/soul-player/output_device.txt")).is_none());
    }

    #[test]
    fn save_then_load_output_exclusive_roundtrips() {
        let dir = std::env::temp_dir().join(format!("soul_rune_excl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("output_exclusive.txt");
        save_exclusive_to(&path, true).unwrap();
        assert!(load_exclusive_from(&path), "exclusive=on reloads as on");
        save_exclusive_to(&path, false).unwrap();
        assert!(
            !load_exclusive_from(&path),
            "exclusive=off reloads as off (a blank file)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_output_exclusive_is_off() {
        assert!(
            !load_exclusive_from(Path::new("/no/such/soul-player/output_exclusive.txt")),
            "a missing file defaults to shared (off)"
        );
    }

    #[test]
    fn save_then_load_replaygain_roundtrips() {
        let dir = std::env::temp_dir().join(format!("soul_rune_rg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("replaygain.txt");
        for (mode, preamp) in [
            (ReplayGainMode::Off, 0.0f32),
            (ReplayGainMode::Track, 3.0),
            (ReplayGainMode::Album, -4.5),
        ] {
            save_rg_to(&path, mode, preamp).unwrap();
            let (m, pa) = load_rg_from(&path);
            assert_eq!(m, mode, "mode reloads verbatim");
            assert!((pa - preamp).abs() < 1e-6, "preamp {pa} != {preamp}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_replaygain_is_default() {
        let (mode, preamp) = load_rg_from(Path::new("/no/such/soul-player/replaygain.txt"));
        assert_eq!(mode, ReplayGainMode::Off);
        assert_eq!(preamp, 0.0);
    }

    fn tl(integrated: f32) -> TrackLoudness {
        TrackLoudness {
            integrated_lufs: integrated,
            lra: 8.5,
            true_peak_dbtp: -1.3,
            plr: 11.2,
            dr: 10.0,
        }
    }

    #[test]
    fn loudness_cache_roundtrips_a_valid_entry() {
        let dir = std::env::temp_dir().join(format!("soul_rune_loud_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("loudness_cache.txt");

        // A real audio-ish file to stamp (content only matters for mtime+size).
        let audio = dir.join("track.flac");
        std::fs::write(&audio, b"some bytes standing in for audio").unwrap();

        record_loudness_to(&cache, &audio, &tl(-4.25)).unwrap();
        let loaded = load_loudness_from(&cache);
        assert_eq!(loaded.len(), 1, "the recorded entry reloads");
        assert_eq!(loaded[0].0, audio, "path reloads verbatim");
        assert_eq!(
            loaded[0].1,
            tl(-4.25),
            "the full metric set reloads verbatim"
        );

        // Upsert the same path → still one entry, the new metrics win (dedup).
        record_loudness_to(&cache, &audio, &tl(1.5)).unwrap();
        let loaded = load_loudness_from(&cache);
        assert_eq!(loaded.len(), 1, "an upsert dedups by path");
        assert_eq!(loaded[0].1, tl(1.5), "the newer metrics win");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loudness_cache_invalidates_a_stale_or_missing_entry() {
        let dir = std::env::temp_dir().join(format!("soul_rune_loud_stale_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("loudness_cache.txt");

        let audio = dir.join("track.flac");
        std::fs::write(&audio, b"original content").unwrap();
        record_loudness_to(&cache, &audio, &tl(-4.25)).unwrap();
        assert_eq!(load_loudness_from(&cache).len(), 1, "fresh entry loads");

        // Change the file's size (and content) → the recorded stamp no longer
        // matches → the stale gain is dropped on load.
        std::fs::write(&audio, b"replaced with different, longer content").unwrap();
        assert!(
            load_loudness_from(&cache).is_empty(),
            "a changed file invalidates its cached gain"
        );

        // A record for a vanished file persists nothing to re-seed later.
        let gone = dir.join("missing.flac");
        record_loudness_to(&cache, &gone, &tl(0.0)).unwrap();
        assert!(
            load_loudness_from(&cache).is_empty(),
            "an un-stat-able file is never cached"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_loudness_cache_is_empty() {
        assert!(
            load_loudness_from(Path::new("/no/such/soul-player/loudness_cache.txt")).is_empty()
        );
    }

    // --- scan cache (warm startup) ---

    /// A minimal valid stereo PCM WAV (silence), enough for the importer to scan
    /// it into one cached track.
    fn tiny_wav() -> Vec<u8> {
        let (rate, frames, channels, bits): (u32, u32, u16, u16) = (48_000, 4_800, 2, 16);
        let block_align = channels * bits / 8;
        let data_len = frames * u32::from(block_align);
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        v
    }

    #[test]
    fn scan_caches_roundtrip_a_populated_cache() {
        use soul_player_host::PlayerHandle;
        let dir = std::env::temp_dir().join(format!("soul_rune_scancache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.wav"), tiny_wav()).unwrap();

        // Produce a *populated* cache via a real host scan, then persist + reload it,
        // proving a non-empty ScanCache survives the on-disk binary format
        // (entry fidelity, not just the map shell).
        let h = PlayerHandle::offline();
        h.set_sources(&[(dir.clone(), true)]).unwrap();
        let snap = h.caches_snapshot();
        assert_eq!(
            snap.get(&dir).map(ScanCache::len),
            Some(1),
            "the host cached the scanned file"
        );

        let file = dir.join("scan_caches.bin");
        save_scan_caches_to(&file, &snap).unwrap();
        let loaded = load_scan_caches_from(&file);
        assert_eq!(
            loaded.get(&dir).map(ScanCache::len),
            Some(1),
            "a populated cache round-trips through the on-disk binary format"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_scan_caches_discards_a_stale_version() {
        let dir =
            std::env::temp_dir().join(format!("soul_rune_scancache_stale_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("scan_caches.bin");

        // A file stamped with an incompatible importer version must be discarded
        // (treated as a cold start) rather than reused with stale fields.
        let stale = PersistedCaches {
            version: SCAN_CACHE_VERSION.wrapping_add(1),
            caches: HashMap::new(),
        };
        std::fs::write(&file, postcard::to_allocvec(&stale).unwrap()).unwrap();
        assert!(
            load_scan_caches_from(&file).is_empty(),
            "a mismatched-version cache is discarded"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pre_multi_disc_v3_scan_cache_is_discarded() {
        // The multi-disc importer bumped the cache to v4 (RawTrack gained disc
        // total/subtitle/MB release id); a v3 file must cold-start, not decode stale.
        assert_eq!(SCAN_CACHE_VERSION, 4, "core multi-disc importer is v4");
        let dir =
            std::env::temp_dir().join(format!("soul_rune_scancache_v3_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("scan_caches.bin");
        let v3 = PersistedCaches {
            version: 3,
            caches: HashMap::new(),
        };
        std::fs::write(&file, postcard::to_allocvec(&v3).unwrap()).unwrap();
        assert!(
            load_scan_caches_from(&file).is_empty(),
            "a v3 cache is discarded"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_scan_caches_is_empty() {
        assert!(
            load_scan_caches_from(Path::new("/no/such/soul-player/scan_caches.bin")).is_empty(),
            "a missing cache file loads as an empty (cold) cache"
        );
    }

    #[test]
    fn indexing_mode_persists() {
        let _g = use_test_config_dir();
        assert_eq!(load_indexing_mode(), "auto", "defaults to auto");
        save_indexing_mode("gentle");
        assert_eq!(load_indexing_mode(), "gentle");
        save_indexing_mode("max");
        assert_eq!(load_indexing_mode(), "max");
    }

    #[test]
    fn window_geometry_persists_through_the_config_file() {
        let _g = use_test_config_dir();
        // A normal window persists its exact rect.
        save_window_geometry((120, 90), (1200, 760), false, false);
        assert_eq!(
            load_window_geometry(),
            Some(WindowGeometry {
                x: 120,
                y: 90,
                w: 1200,
                h: 760,
                maximized: false
            })
        );
        // Quitting maximized keeps the stored normal rect, only flipping the flag.
        save_window_geometry((0, 0), (3840, 2160), true, false);
        assert_eq!(
            load_window_geometry(),
            Some(WindowGeometry {
                x: 120,
                y: 90,
                w: 1200,
                h: 760,
                maximized: true
            })
        );
        // A minimized window must not overwrite the saved rect.
        save_window_geometry((7, 7), (50, 50), false, true);
        assert_eq!(
            load_window_geometry(),
            Some(WindowGeometry {
                x: 120,
                y: 90,
                w: 1200,
                h: 760,
                maximized: true
            })
        );
    }

    #[test]
    fn restore_window_geometry_returns_the_saved_rect() {
        let _g = use_test_config_dir();
        save_window_geometry((42, 24), (900, 600), false, false);
        // In a test build `displays()` is empty, so the clamp is a pass-through and
        // restore returns exactly what was saved (the clamp itself is covered by the
        // `clamp_*` cases above). This exercises the load → clamp restore wiring.
        assert_eq!(
            restore_window_geometry(),
            Some(WindowGeometry {
                x: 42,
                y: 24,
                w: 900,
                h: 600,
                maximized: false
            })
        );
    }
}
