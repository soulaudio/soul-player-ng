//! Off-thread helpers for the managed-library "Add music → organize" flow: the
//! filesystem scan that feeds [`organize::plan`](soul_player_host::organize::plan),
//! and the preset-id → template-string lookup. Both are pure (no UI state), so the
//! menu/apply wiring can run them on a worker thread and post results back via
//! `AppHandle::update`.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use soul_player_host::organize;

/// Audio file extensions the organizer accepts, lowercased. This mirrors core's
/// `soul-audio-import::SUPPORTED` set, duplicated here because that constant is
/// not re-exported through `soul-player-host`; keep the two in sync if core gains
/// or drops a format.
const SUPPORTED_EXTS: &[&str] = &[
    "wav", "flac", "mp3", "mp2", "mp1", "m4a", "aac", "ogg", "oga", "opus", "aiff", "aif", "dsf",
    "dff", "mka", "webm", "wv", "caf",
];

/// Max directory depth [`scan_audio`] descends, a guard against symlink loops and
/// pathologically nested trees.
const MAX_DEPTH: usize = 64;

/// Recursively collect audio files under `dir` whose lowercased extension is in
/// [`SUPPORTED_EXTS`]. Hidden entries (names starting with `.`) are skipped, both
/// dotfiles and dot-directories, and descent stops at [`MAX_DEPTH`]. Unreadable
/// directories are silently skipped rather than aborting the walk. Order is the
/// platform's directory order; [`organize::plan`] does not depend on it.
pub(crate) fn scan_audio(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    scan_into(dir, 0, &mut out);
    out
}

fn scan_into(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // Skip hidden dotfiles / dot-directories (use lossy so a non-UTF-8 name
        // isn't dropped for the hidden check, only a leading '.' hides it).
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => scan_into(&path, depth + 1, out),
            Ok(_) if is_supported(&path) => out.push(path),
            _ => {}
        }
    }
}

/// Whether `path`'s extension (case-insensitive) is a supported audio format.
fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| SUPPORTED_EXTS.contains(&e.as_str()))
}

/// The path-template string for a managed-library preset id (including the
/// `+disc-folders` variants), via [`organize::preset`]. Falls back to the first
/// preset when `id` is unknown (e.g. a persisted id from a build that has since
/// renamed its presets).
pub(crate) fn template_for(id: &str) -> &'static str {
    organize::preset(id).unwrap_or(organize::presets()[0].1)
}

/// The suffix of a preset's disc-sub-folder variant (`artist-album+disc-folders`).
pub(crate) const DISC_FOLDERS: &str = "+disc-folders";

/// The Layout dropdown's presets: every organizer preset except the
/// `+disc-folders` variants (the Multi-disc albums dropdown picks those).
pub(crate) fn layout_presets() -> Vec<&'static str> {
    organize::presets()
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !id.ends_with(DISC_FOLDERS))
        .collect()
}

/// A stored template id as `(layout preset, disc sub-folders?)`.
pub(crate) fn split_template_id(id: &str) -> (&str, bool) {
    match id.strip_suffix(DISC_FOLDERS) {
        Some(layout) => (layout, true),
        None => (id, false),
    }
}

/// Whether `layout` has a `+disc-folders` variant (`flat` has none: no album folder).
pub(crate) fn has_disc_folder_variant(layout: &str) -> bool {
    organize::preset(&format!("{layout}{DISC_FOLDERS}")).is_some()
}

/// The stored id for a layout + multi-disc choice: `<layout>+disc-folders` when
/// chosen and available, else `<layout>`.
pub(crate) fn compose_template_id(layout: &str, disc_folders: bool) -> String {
    if disc_folders && has_disc_folder_variant(layout) {
        format!("{layout}{DISC_FOLDERS}")
    } else {
        layout.to_string()
    }
}

/// Migrate a persisted template id: a known preset id is kept (the pre-multi-disc
/// ids are still presets, now disc-safe); an unknown `+disc-folders` id keeps its
/// base layout; anything else falls back to the first preset.
pub(crate) fn normalize_template_id(id: &str) -> String {
    if organize::preset(id).is_some() {
        return id.to_string();
    }
    let (layout, _) = split_template_id(id);
    if organize::preset(layout).is_some() {
        layout.to_string()
    } else {
        organize::presets()[0].0.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty temp directory unique to this process + `tag` (the crate's
    /// tests avoid the `tempfile` dependency, see `config.rs`).
    fn fresh_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("soul_rune_scan_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_audio_collects_only_supported_nondot_files() {
        let root = fresh_dir("mixed");
        // Supported audio at the top level (one with an UPPERCASE extension).
        std::fs::write(root.join("a.flac"), b"x").unwrap();
        std::fs::write(root.join("b.MP3"), b"x").unwrap();
        // Supported audio nested one level down.
        let sub = root.join("Album");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("c.wav"), b"x").unwrap();
        // Non-audio files: excluded by extension.
        std::fs::write(root.join("notes.txt"), b"x").unwrap();
        std::fs::write(root.join("cover.jpg"), b"x").unwrap();
        // A dotfile with an audio extension, excluded as hidden.
        std::fs::write(root.join(".hidden.flac"), b"x").unwrap();

        let mut names: Vec<String> = scan_audio(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["a.flac", "b.MP3", "c.wav"],
            "only non-hidden supported audio is collected, recursively + case-insensitively"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_audio_skips_dot_directories() {
        let root = fresh_dir("dotdir");
        let hidden_dir = root.join(".Trash");
        std::fs::create_dir(&hidden_dir).unwrap();
        std::fs::write(hidden_dir.join("ghost.flac"), b"x").unwrap();
        std::fs::write(root.join("real.flac"), b"x").unwrap();

        let names: Vec<String> = scan_audio(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["real.flac"], "a dot-directory is not descended");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn template_for_resolves_known_and_falls_back() {
        assert_eq!(template_for("flat"), "{Artist} - {Title}");
        // Core made every album preset disc-safe: `{DiscPrefix}` is "" on a
        // single-disc album, so single-disc output is byte-identical to before.
        assert_eq!(
            template_for("artist-album"),
            "{AlbumArtist}/{Album}/{DiscPrefix}{TrackNo} - {Title}"
        );
        assert_eq!(
            template_for("artist-album+disc-folders"),
            "{AlbumArtist}/{Album}/{DiscDir}/{TrackNo} - {Title}"
        );
        assert_eq!(
            template_for("not-a-real-preset"),
            organize::presets()[0].1,
            "an unknown id falls back to the first preset"
        );
    }

    #[test]
    fn layout_presets_hide_the_disc_folder_variants() {
        assert_eq!(
            layout_presets(),
            [
                "artist-album",
                "artist-year-album",
                "genre-artist-album",
                "flat"
            ]
        );
    }

    #[test]
    fn template_ids_split_and_compose() {
        assert_eq!(split_template_id("artist-album"), ("artist-album", false));
        assert_eq!(
            split_template_id("artist-album+disc-folders"),
            ("artist-album", true)
        );
        assert_eq!(
            compose_template_id("genre-artist-album", true),
            "genre-artist-album+disc-folders"
        );
        assert_eq!(
            compose_template_id("genre-artist-album", false),
            "genre-artist-album"
        );
        assert_eq!(
            compose_template_id("flat", true),
            "flat",
            "flat has no disc-folder variant"
        );
        assert!(has_disc_folder_variant("artist-year-album"));
        assert!(!has_disc_folder_variant("flat"));
    }

    #[test]
    fn stored_template_ids_migrate() {
        // Pre-multi-disc ids stay valid (core made them disc-safe).
        assert_eq!(normalize_template_id("artist-album"), "artist-album");
        assert_eq!(normalize_template_id("flat"), "flat");
        assert_eq!(
            normalize_template_id("artist-album+disc-folders"),
            "artist-album+disc-folders"
        );
        // A variant that does not exist keeps its base; garbage falls back.
        assert_eq!(normalize_template_id("flat+disc-folders"), "flat");
        assert_eq!(normalize_template_id("bogus"), "artist-album");
        assert_eq!(normalize_template_id(""), "artist-album");
    }
}
