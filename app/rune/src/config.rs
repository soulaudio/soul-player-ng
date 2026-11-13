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

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use soul_player_host::ReplayGainMode;

use crate::views::sidebar::{DEFAULT_WIDTH, MAX_WIDTH, MIN_WIDTH};

/// Test-only override for the config directory, so `cargo test` never reads or
/// writes the user's real settings. `None` (and compiled out) in production.
#[cfg(test)]
static TEST_CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Point config persistence at a throwaway temp directory for the rest of this
/// test process (idempotent — first call wins). Call it at the start of any test
/// that writes a setting, so tests never clobber the user's real `config.toml`.
#[cfg(test)]
pub(crate) fn use_test_config_dir() {
    TEST_CONFIG_DIR.get_or_init(|| {
        std::env::temp_dir().join(format!("soul-player-test-{}", std::process::id()))
    });
}

/// The app's config directory — `<os-config>/soul-player`, or the test override
/// when one is active (see [`use_test_config_dir`]). All persisted files hang off
/// this, so pointing it elsewhere isolates every `load_*`/`save_*`.
fn soul_player_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_CONFIG_DIR.get() {
        return Some(dir.clone());
    }
    Some(dirs::config_dir()?.join("soul-player"))
}

/// `<config>/soul-player/config.toml` — the one settings file.
fn config_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("config.toml"))
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
    /// Primary nav items the user has hidden (by id: `home`/`albums`/`artists`/
    /// `tracks`). Empty = all shown. Storing the *hidden* set means a new nav
    /// item added in a later build defaults to visible.
    nav_hidden: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            theme: "system".into(),
            sidebar_width: DEFAULT_WIDTH,
            sidebar_collapsed: false,
            locale: "system".into(),
            nav_hidden: Vec::new(),
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

// --- output device preference ---
// (Kept as its own `output_device.txt` rather than folded into `config.toml`;
// a follow-up can migrate it, but the behaviour is unchanged.)

/// `<config>/soul-player/output_device.txt` — the persisted output-device choice.
/// A blank / missing file means "system default".
pub fn output_device_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("output_device.txt"))
}

/// The saved output-device name, or `None` for the system default (also `None` if
/// the file is missing / unreadable / blank).
pub fn load_output_device() -> Option<String> {
    output_device_path().and_then(|p| load_device_from(&p))
}

/// Persist the chosen output device (best-effort — a failed write never breaks the
/// UI). `None` (or a name that trims to empty) writes an empty file, meaning
/// "system default" — the file is emptied rather than deleted so the choice stays
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

// --- ReplayGain preference ---

/// `<config>/soul-player/replaygain.txt` — the persisted ReplayGain choice, two
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

/// Persist the ReplayGain choice (best-effort — a failed write never breaks the UI).
pub fn save_replaygain(mode: ReplayGainMode, preamp_db: f32) {
    if let Some(p) = replaygain_path() {
        let _ = save_rg_to(&p, mode, preamp_db);
    }
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

/// `<config>/soul-player/loudness_cache.txt` — the persisted computed gains for
/// previously-untagged tracks the backend background-analyzed. One line per entry:
/// `"<gain_db>\t<mtime_secs>\t<size>\t<path>"`. The mtime+size are a cheap
/// freshness stamp: an entry whose file no longer matches (edited / replaced /
/// moved) is dropped on load so a stale gain is never seeded.
pub fn loudness_cache_path() -> Option<PathBuf> {
    Some(soul_player_dir()?.join("loudness_cache.txt"))
}

/// The saved, still-valid `(path, gain_db)` entries (empty if the file is missing
/// / unreadable). Each is validated against the file's CURRENT mtime+size; stale
/// or vanished files are skipped.
pub fn load_loudness_cache() -> Vec<(PathBuf, f32)> {
    loudness_cache_path()
        .map(|p| load_loudness_from(&p))
        .unwrap_or_default()
}

/// Record a freshly-computed gain for `audio_path` (best-effort — a failed write
/// never breaks the UI). Upserts by path (the new stamp wins) and rewrites the
/// file, so the cache dedups rather than growing without bound.
pub fn record_loudness(audio_path: &Path, gain_db: f32) {
    if let Some(p) = loudness_cache_path() {
        let _ = record_loudness_to(&p, audio_path, gain_db);
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

/// Read + validate the cache from `path` (the testable core). A line parses as
/// `gain \t mtime \t size \t path` (the path may itself contain tabs — only the
/// first three tabs are split on), and is kept only when the file still matches
/// the recorded mtime+size.
fn load_loudness_from(path: &Path) -> Vec<(PathBuf, f32)> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in body.lines() {
        if line.is_empty() {
            continue;
        }
        // Split off exactly the first three fields; the rest is the path verbatim.
        let mut it = line.splitn(4, '\t');
        let (Some(gain), Some(mtime), Some(size), Some(p)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        let (Ok(gain), Ok(mtime), Ok(size)) = (
            gain.parse::<f32>(),
            mtime.parse::<u64>(),
            size.parse::<u64>(),
        ) else {
            continue;
        };
        let audio = PathBuf::from(p);
        if file_stamp(&audio) == Some((mtime, size)) {
            out.push((audio, gain));
        }
    }
    out
}

/// Upsert `(audio_path, gain_db)` into the cache at `cache_path` and rewrite it
/// (the testable core of [`record_loudness`]). Skips a non-UTF-8 or un-stat-able
/// audio path (nothing to validate against later). Existing entries are reloaded
/// WITHOUT revalidation so a momentarily-offline file isn't purged by a write for
/// a different track.
fn record_loudness_to(cache_path: &Path, audio_path: &Path, gain_db: f32) -> std::io::Result<()> {
    let Some((mtime, size)) = file_stamp(audio_path) else {
        return Ok(()); // can't stamp it → nothing worth persisting
    };
    let Some(path_str) = audio_path.to_str() else {
        return Ok(()); // non-UTF-8 path → skip rather than corrupt the file
    };
    if let Some(dir) = cache_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Reload existing lines verbatim (key = path field) so the upsert dedups.
    let mut lines: Vec<(String, String)> = Vec::new(); // (path_key, full_line)
    if let Ok(body) = std::fs::read_to_string(cache_path) {
        for line in body.lines() {
            if line.is_empty() {
                continue;
            }
            if let Some(key) = line.splitn(4, '\t').nth(3) {
                lines.push((key.to_string(), line.to_string()));
            }
        }
    }
    let fresh = format!("{gain_db}\t{mtime}\t{size}\t{path_str}");
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
            nav_hidden: vec!["home".into()],
        };
        let body = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&body).unwrap();
        assert_eq!(back.sources, cfg.sources);
        assert_eq!(back.theme, "dark");
        assert_eq!(back.sidebar_width, 300.0);
        assert!(back.sidebar_collapsed);
        assert_eq!(back.locale, "de");
        assert_eq!(back.nav_hidden, vec!["home".to_string()]);
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
        // None (and a blank name) persist as the system default — a blank file.
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

    #[test]
    fn loudness_cache_roundtrips_a_valid_entry() {
        let dir = std::env::temp_dir().join(format!("soul_rune_loud_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = dir.join("loudness_cache.txt");

        // A real audio-ish file to stamp (content only matters for mtime+size).
        let audio = dir.join("track.flac");
        std::fs::write(&audio, b"some bytes standing in for audio").unwrap();

        record_loudness_to(&cache, &audio, -4.25).unwrap();
        let loaded = load_loudness_from(&cache);
        assert_eq!(loaded.len(), 1, "the recorded entry reloads");
        assert_eq!(loaded[0].0, audio, "path reloads verbatim");
        assert!(
            (loaded[0].1 - (-4.25)).abs() < 1e-6,
            "gain reloads verbatim"
        );

        // Upsert the same path → still one entry, new gain wins (dedup).
        record_loudness_to(&cache, &audio, 1.5).unwrap();
        let loaded = load_loudness_from(&cache);
        assert_eq!(loaded.len(), 1, "an upsert dedups by path");
        assert!((loaded[0].1 - 1.5).abs() < 1e-6, "the newer gain wins");

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
        record_loudness_to(&cache, &audio, -4.25).unwrap();
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
        record_loudness_to(&cache, &gone, 0.0).unwrap();
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
}
