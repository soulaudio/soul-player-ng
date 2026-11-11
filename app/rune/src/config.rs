//! Settings persistence: one structured `config.toml` in the user's config dir,
//! (de)serialised via serde — the best-practice shape (a single typed file in the
//! OS config dir) replacing the earlier scattered plain-text files, which this
//! migrates from on first run. App-level on purpose: it keeps `soul-player-host`
//! pure (no I/O, no config-path knowledge). Best-effort throughout — a missing or
//! unreadable config loads as defaults, and a failed save never breaks the UI.
//!
//! The per-setting `load_*`/`save_*` API is kept; each `save_*` does a
//! load-modify-write of the one file (it's tiny, and the UI is single-threaded).
#![forbid(unsafe_code)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::views::sidebar::{DEFAULT_WIDTH, MAX_WIDTH, MIN_WIDTH};

/// `<config>/soul-player/config.toml` — the one settings file.
fn config_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("soul-player").join("config.toml"))
}

/// The on-disk settings. `#[serde(default)]` fills any field a config written by
/// an older build omits, so new settings never break an existing file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    /// Watched music folders (stored as strings — `PathBuf` isn't TOML-native).
    sources: Vec<String>,
    /// Theme preference: `system` | `light` | `dark`.
    theme: String,
    /// Resizable sidebar width (clamped to the legal range on load).
    sidebar_width: f32,
    /// Whether the sidebar is collapsed to its rail.
    sidebar_collapsed: bool,
    /// UI language preference: `system` | `en` | `de` | `es` | `zh` | `cs`.
    locale: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            theme: "system".into(),
            sidebar_width: DEFAULT_WIDTH,
            sidebar_collapsed: false,
            locale: "system".into(),
        }
    }
}

/// Load the config — defaults if missing/garbled, migrating the pre-TOML
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
    let dir = dirs::config_dir()?.join("soul-player");
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

/// The saved watched folders (empty if none).
pub fn load_sources() -> Vec<PathBuf> {
    load().sources.into_iter().map(PathBuf::from).collect()
}

/// Persist the watched folders. Only UTF-8-representable paths are saved — the
/// rare non-UTF-8 path is skipped rather than corrupting the file.
pub fn save_sources(folders: &[PathBuf]) {
    let mut cfg = load();
    cfg.sources = folders
        .iter()
        .filter_map(|p| p.to_str().map(String::from))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_toml_roundtrips() {
        let cfg = Config {
            sources: vec!["/music/a".into(), "/music/b".into()],
            theme: "dark".into(),
            sidebar_width: 300.0,
            sidebar_collapsed: true,
            locale: "de".into(),
        };
        let body = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&body).unwrap();
        assert_eq!(back.sources, cfg.sources);
        assert_eq!(back.theme, "dark");
        assert_eq!(back.sidebar_width, 300.0);
        assert!(back.sidebar_collapsed);
        assert_eq!(back.locale, "de");
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

    #[test]
    fn out_of_range_width_clamps() {
        let wide = Config {
            sidebar_width: 999.0,
            ..Config::default()
        };
        assert_eq!(wide.sidebar_width.clamp(MIN_WIDTH, MAX_WIDTH), MAX_WIDTH);
    }
}
