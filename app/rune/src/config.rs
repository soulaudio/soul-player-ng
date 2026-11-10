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
}
