//! Persist the watched-folder **set** across launches: a newline-separated list
//! of paths under the user's config dir. App-level on purpose — this keeps
//! `soul-player-host` pure (no I/O, no config-path knowledge), and the Rune head
//! is the only head today. Best-effort throughout: a missing or unreadable
//! config loads as empty, and a failed save never breaks the UI.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use soul_player_host::ReplayGainMode;

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

// --- output device preference ---

/// `<config>/soul-player/output_device.txt` — the persisted output-device choice.
/// A blank / missing file means "system default".
pub fn output_device_path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("soul-player")
            .join("output_device.txt"),
    )
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
    Some(
        dirs::config_dir()?
            .join("soul-player")
            .join("replaygain.txt"),
    )
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
}
