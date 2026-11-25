//! Off-thread helpers for the managed-library "Add music → organize" flow: the
//! filesystem scan, planning with the picked folder as the organizer's source
//! root ([`plan_import`]), applying ([`run_apply`]), the Confirm modal's
//! precomputed [`PlanSummary`], and the preset-id → template-string lookup. All
//! are pure (no UI state), so the menu/apply wiring can run them on a worker
//! thread and post results back via `AppHandle::update`.
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
/// directories are silently skipped rather than aborting the walk. Sorted, so the
/// plan (whose ` (2)` numbering of same-title albums follows input order) is
/// deterministic across platforms and re-runs.
pub(crate) fn scan_audio(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    scan_into(dir, 0, &mut out);
    out.sort();
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

/// Scan `src_dir` (the folder the user picked) and plan organizing it into `dest`
/// with the stored template id. `src_dir` is the plan's source root, which is
/// what lets the organizer carry each album's side files (logs, cues, covers,
/// scans) and, on Move, prune the folders it empties (never `src_dir` itself).
pub(crate) fn plan_import(
    src_dir: &Path,
    dest: &Path,
    template_id: &str,
    strategy: organize::Strategy,
) -> organize::OrganizePlan {
    let files = scan_audio(src_dir);
    organize::plan_with(
        &files,
        dest,
        &organize::Template::new(template_for(template_id)),
        strategy,
        &organize::PlanOptions::new(src_dir),
    )
}

/// Apply `plan`, streaming progress; a catastrophic error becomes one failed
/// entry on the destination so the Done tally always shows something.
pub(crate) fn run_apply(
    plan: &organize::OrganizePlan,
    on_progress: impl FnMut(organize::Progress),
) -> organize::Summary {
    organize::apply(plan, on_progress).unwrap_or_else(|e| organize::Summary {
        failed: vec![(plan.dest_root.clone(), e.to_string())],
        ..Default::default()
    })
}

/// How many of a left-in-place album's file names [`summarize`] keeps.
pub(crate) const LEFT_NAMES: usize = 3;

/// An album whose side files stay in the source (shown on the Confirm modal).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LeftAlbum {
    /// The source album folder's name.
    pub folder: String,
    pub reason: organize::SideFilesLeft,
    /// How many files stay (exact).
    pub files: usize,
    /// The first [`LEFT_NAMES`] of them, relative to the album folder.
    pub names: Vec<String>,
}

/// What the Confirm modal shows for a plan, computed once when the plan lands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PlanSummary {
    pub audio: usize,
    pub side: usize,
    pub copy: usize,
    pub moved: usize,
    pub dup: usize,
    pub conflict: usize,
    /// Albums whose side files stay in place, with the reason.
    pub left: Vec<LeftAlbum>,
    /// Albums whose disc numbers come only from `CD1/`-style folder names, which
    /// the chosen one-folder layout loses ([`organize::OrganizePlan::folder_inferred_discs`]).
    pub folder_inferred: usize,
}

/// Tally a plan by kind and action, and list the albums that keep side files in place.
pub(crate) fn summarize(plan: &organize::OrganizePlan) -> PlanSummary {
    use organize::{Action, EntryKind};
    let mut s = PlanSummary::default();
    for e in &plan.entries {
        // `EntryKind` is non-exhaustive: an unknown future kind is neither.
        match e.kind {
            EntryKind::Audio => s.audio += 1,
            EntryKind::SideFile => s.side += 1,
            _ => {}
        }
        match e.action {
            Action::Copy => s.copy += 1,
            Action::Move => s.moved += 1,
            Action::SkipDuplicate => s.dup += 1,
            Action::SkipConflict => s.conflict += 1,
        }
    }
    s.left = plan.side_files_left.iter().map(left_album).collect();
    s.folder_inferred = plan.folder_inferred_discs.len();
    s
}

/// One [`organize::LeftInPlace`] as the modal lists it.
fn left_album(l: &organize::LeftInPlace) -> LeftAlbum {
    let name = |p: &PathBuf| {
        p.strip_prefix(&l.album_root)
            .unwrap_or(p)
            .display()
            .to_string()
    };
    LeftAlbum {
        folder: l.album_root.file_name().map_or_else(
            || l.album_root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        reason: l.reason,
        files: l.files,
        names: l.paths.iter().take(LEFT_NAMES).map(name).collect(),
    }
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
    fn scan_audio_is_sorted() {
        let root = fresh_dir("sorted");
        for n in ["c.flac", "a.flac", "b.flac"] {
            std::fs::write(root.join(n), b"x").unwrap();
        }
        let names: Vec<String> = scan_audio(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.flac", "b.flac", "c.flac"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `src/Album/{01.flac, album.log}` + `src/{loose.flac, notes.txt}`, planned
    /// into `root/dst`. Returns `(root, src, dst)`.
    fn album_fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = fresh_dir(tag);
        let src = root.join("src");
        let album = src.join("Album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01.flac"), b"x").unwrap();
        std::fs::write(album.join("album.log"), b"log").unwrap();
        std::fs::write(src.join("loose.flac"), b"y").unwrap();
        std::fs::write(src.join("notes.txt"), b"n").unwrap();
        (root.clone(), src, root.join("dst"))
    }

    /// The picked folder is the plan's source root: an album folder's side files
    /// travel with it, while files loose in the picked folder stay put (reported
    /// with their reason and names).
    #[test]
    fn plan_import_uses_the_picked_folder_as_source_root() {
        let (root, src, dst) = album_fixture("plan_copy");
        let plan = plan_import(&src, &dst, "artist-album", organize::Strategy::Copy);
        let s = summarize(&plan);
        assert_eq!(
            (s.audio, s.side),
            (2, 1),
            "album.log travels with its album"
        );
        assert_eq!((s.copy, s.moved, s.dup, s.conflict), (3, 0, 0, 0));
        assert_eq!(
            s.left,
            [LeftAlbum {
                folder: "src".into(),
                reason: organize::SideFilesLeft::AtSourceRoot,
                files: 1,
                names: vec!["notes.txt".into()],
            }]
        );
        assert_eq!(s.folder_inferred, 0);
        assert!(plan.prune_dirs.is_empty(), "Copy never prunes");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Move carries the album's side files, empties its folder, and the apply
    /// prunes it (never the picked folder itself).
    #[test]
    fn move_prunes_emptied_album_folders() {
        let (root, src, dst) = album_fixture("plan_move");
        let plan = plan_import(&src, &dst, "artist-album", organize::Strategy::Move);
        let s = summarize(&plan);
        assert_eq!((s.copy, s.moved), (0, 3));
        let mut ticks = 0;
        let summary = run_apply(&plan, |_| ticks += 1);
        assert_eq!(ticks, plan.entries.len());
        assert_eq!((summary.moved, summary.failed.len()), (3, 0));
        assert_eq!(summary.pruned_dirs, 1, "the emptied Album folder is pruned");
        assert!(!src.join("Album").exists());
        assert!(src.join("notes.txt").exists(), "loose files stay in place");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Disc numbers that come only from `CD1/`/`CD2/` folder names are lost by the
    /// one-folder layout (counted for the warning), kept by the disc sub-folders.
    #[test]
    fn folder_inferred_discs_are_counted_for_the_one_folder_layout() {
        let root = fresh_dir("discs");
        let src = root.join("src");
        for cd in ["CD1", "CD2"] {
            let d = src.join("Box").join(cd);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("01.flac"), cd.as_bytes()).unwrap();
        }
        let dst = root.join("dst");
        let one = plan_import(&src, &dst, "artist-album", organize::Strategy::Copy);
        assert_eq!(summarize(&one).folder_inferred, 1);
        let sub = plan_import(
            &src,
            &dst,
            "artist-album+disc-folders",
            organize::Strategy::Copy,
        );
        assert_eq!(summarize(&sub).folder_inferred, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// At most [`LEFT_NAMES`] file names are kept per album (the count stays exact).
    #[test]
    fn left_album_names_are_capped() {
        let root = fresh_dir("names");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("loose.flac"), b"x").unwrap();
        for n in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
            std::fs::write(src.join(n), b"t").unwrap();
        }
        let plan = plan_import(&src, &root.join("dst"), "flat", organize::Strategy::Copy);
        let s = summarize(&plan);
        assert_eq!(s.left.len(), 1);
        assert_eq!(s.left[0].files, 5);
        assert_eq!(s.left[0].names, ["a.txt", "b.txt", "c.txt"]);
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
