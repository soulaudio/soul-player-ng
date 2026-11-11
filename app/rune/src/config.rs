//! Persist the watched-folder **set** across launches: a newline-separated list
//! of paths under the user's config dir. App-level on purpose — this keeps
//! `soul-player-host` pure (no I/O, no config-path knowledge), and the Rune head
//! is the only head today. Best-effort throughout: a missing or unreadable
//! config loads as empty, and a failed save never breaks the UI.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// `<config>/soul-player/sources.txt` — where the watched set is saved.
pub fn sources_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("soul-player").join("sources.txt"))
}

/// The saved watched folders (empty if the file is missing/unreadable).
pub fn load_sources() -> Vec<PathBuf> {
    sources_path().map(|p| load_from(&p)).unwrap_or_default()
}

/// Persist the watched folders (best-effort — errors are swallowed).
pub fn save_sources(folders: &[PathBuf]) {
    if let Some(p) = sources_path() {
        let _ = save_to(&p, folders);
    }
}

/// Read a newline-separated path list from `path` (the testable core).
fn load_from(path: &Path) -> Vec<PathBuf> {
    match std::fs::read_to_string(path) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Write `folders` to `path` as a newline-separated list, creating the parent
/// dir. Only UTF-8-representable paths are saved — the rare non-UTF-8 path is
/// skipped rather than corrupting the file.
fn save_to(path: &Path, folders: &[PathBuf]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = folders
        .iter()
        .filter_map(|p| p.to_str())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, body)
}

// --- theme preference ---

/// `<config>/soul-player/theme.txt` — the persisted light/dark preference.
fn theme_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("soul-player").join("theme.txt"))
}

/// The saved theme preference string ("system"/"light"/"dark"), or `None`.
pub fn load_theme_pref() -> Option<String> {
    let p = theme_path()?;
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Persist the theme preference (best-effort — a failed write never breaks the UI).
pub fn save_theme_pref(pref: &str) {
    if let Some(p) = theme_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, pref);
    }
}

// --- sidebar geometry ---

/// `<config>/soul-player/sidebar.txt` — the resizable sidebar's width (line 1)
/// and collapsed flag (line 2). Mirrors the old app's persisted
/// `sidebar-width` / `sidebar-collapsed` localStorage keys.
fn sidebar_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("soul-player").join("sidebar.txt"))
}

/// Restore the sidebar geometry as `(width, collapsed)`. The width is clamped to
/// the legal range; a missing or garbled file yields the default (expanded).
pub fn load_sidebar() -> (f32, bool) {
    sidebar_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| parse_sidebar(&s))
        .unwrap_or((crate::views::sidebar::DEFAULT_WIDTH, false))
}

/// Persist the sidebar geometry (best-effort — a failed write never breaks the UI).
pub fn save_sidebar(width: f32, collapsed: bool) {
    if let Some(p) = sidebar_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, format_sidebar(width, collapsed));
    }
}

/// Parse the two-line sidebar file; the testable core. The width clamps to
/// `[MIN_WIDTH, MAX_WIDTH]`, and an unparseable width falls back to the default.
fn parse_sidebar(body: &str) -> (f32, bool) {
    use crate::views::sidebar::{DEFAULT_WIDTH, MAX_WIDTH, MIN_WIDTH};
    let mut lines = body.lines();
    let width = lines
        .next()
        .and_then(|l| l.trim().parse::<f32>().ok())
        .filter(|w| w.is_finite())
        .map(|w| w.clamp(MIN_WIDTH, MAX_WIDTH))
        .unwrap_or(DEFAULT_WIDTH);
    let collapsed = lines
        .next()
        .map(|l| l.trim() == "collapsed")
        .unwrap_or(false);
    (width, collapsed)
}

/// Serialise the sidebar geometry back to its two-line form.
fn format_sidebar(width: f32, collapsed: bool) -> String {
    format!(
        "{width}\n{}",
        if collapsed { "collapsed" } else { "expanded" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_roundtrips() {
        let dir = std::env::temp_dir().join(format!("soul_rune_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("sources.txt");
        let folders = vec![PathBuf::from("/music/a"), PathBuf::from("/music/b")];
        save_to(&path, &folders).unwrap();
        assert_eq!(load_from(&path), folders, "the saved set reloads verbatim");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_is_empty() {
        assert!(load_from(Path::new("/no/such/soul-player/sources.txt")).is_empty());
    }

    #[test]
    fn sidebar_roundtrips_and_clamps() {
        use crate::views::sidebar::{DEFAULT_WIDTH, MAX_WIDTH, MIN_WIDTH};
        // An in-range width and the collapsed flag survive a round-trip.
        assert_eq!(parse_sidebar(&format_sidebar(300.0, true)), (300.0, true));
        assert_eq!(parse_sidebar(&format_sidebar(288.0, false)), (288.0, false));
        // Out-of-range widths clamp to the legal bounds.
        assert_eq!(parse_sidebar("999\nexpanded").0, MAX_WIDTH);
        assert_eq!(parse_sidebar("10\nexpanded").0, MIN_WIDTH);
        // Garbage falls back to the default, not collapsed.
        assert_eq!(parse_sidebar("garbage"), (DEFAULT_WIDTH, false));
    }
}
