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

#[cfg(test)]
mod e2e {
    //! The managed-library flow end to end over a Genshin-shaped staging folder
    //! (the real Soulseek download's shapes: a flat disc-tagged album with an EAC
    //! log + cue and OS junk, a `Disc01/Disc02` album with a disc subtitle and an
    //! MBID, a `CD1/CD2` album whose discs come only from the folder names, and
    //! two same-title editions in different folders), organized through the SAME
    //! helpers the app's worker threads run (`plan_import` -> `run_apply`), for
    //! Copy and Move x one-folder and disc-sub-folder layouts. Each run checks the
    //! exact destination tree and bytes, re-run idempotence, Move pruning, then
    //! watches the destination like `organize_confirm` does and asserts the
    //! Albums model and the album page's disc headers.

    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use rune::{Env, Runtime};
    use soul_player_host::PlayerHandle;
    use soul_player_host::organize::Strategy;

    use super::{plan_import, run_apply, summarize};
    use crate::test_fixtures::{flac_with_comments, put};
    use crate::{AlbumRef, Route, State};

    const ARTIST: &str = "HOYO-MiX";
    const JADE: &str = "Jade Moon Upon a Sea of Clouds";
    const FOREST: &str = "Genshin Impact - Forest of Jnana and Vidya (Original Game Soundtrack)";
    const FOOTPRINTS: &str = "Footprints of the Traveler";
    const VOYAGE: &str = "The Shimmering Voyage";
    const FOREST_MBID: &str = "0d6c2b4e-6f61-4bd5-9f2c-8a1f3c3e2a10";

    /// A fresh, empty temp directory unique to this process + `tag`.
    fn fresh(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("soul_rune_org_e2e_{}_{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A HOYO-MiX track tagged like the real download, plus `extra` tags.
    fn track(album: &str, title: &str, n: u32, extra: &[&str]) -> Vec<u8> {
        let mut c = vec![
            format!("ALBUM={album}"),
            format!("TITLE={title}"),
            format!("ARTIST={ARTIST}"),
            format!("ALBUMARTIST={ARTIST}"),
            "DATE=2022".to_owned(),
            "GENRE=Game".to_owned(),
            format!("TRACKNUMBER={n}"),
        ];
        c.extend(extra.iter().map(|s| (*s).to_owned()));
        let refs: Vec<&str> = c.iter().map(String::as_str).collect();
        flac_with_comments(&refs)
    }

    /// One staged file: its source path, its destination in the one-folder and
    /// disc-sub-folder layouts (relative to the managed root), and its bytes.
    struct Staged {
        src: String,
        one: String,
        discs: String,
        bytes: Vec<u8>,
    }

    fn staged(src: String, one: String, discs: String, bytes: Vec<u8>) -> Staged {
        Staged {
            src,
            one,
            discs,
            bytes,
        }
    }

    /// The staging library, nested like the real downloads (`Flac/<artist>/..`).
    /// Footprints' two editions sort plain-first, so the Chinese edition is the
    /// one that gets ` (2)` (the organizer numbers same-title albums in input order).
    fn staging() -> Vec<Staged> {
        let j_src = format!("Flac/{ARTIST}/2020.11.06 {JADE}");
        let j = format!("{ARTIST}/{JADE}");
        let f_src = format!("Flac/{ARTIST}/Forest of Jnana and Vidya");
        let f = format!("{ARTIST}/{FOREST}");
        let fp_src = format!("Flac/{ARTIST}/{FOOTPRINTS}");
        let fp = format!("{ARTIST}/{FOOTPRINTS}");
        let v_src = format!("Flac/{ARTIST}/{VOYAGE}");
        let v = format!("{ARTIST}/{VOYAGE}");
        let sumeru = "DISCSUBTITLE=Battles of Sumeru";
        let mbid = format!("MUSICBRAINZ_ALBUMID={FOREST_MBID}");
        // A file whose destination is the same in both layouts (album root, one disc).
        let same_in_both =
            |src: String, dst: String, bytes: &[u8]| staged(src, dst.clone(), dst, bytes.to_vec());
        vec![
            // Flat, disc-tagged (`DISCNUMBER=n/total`), files numbered across discs.
            staged(
                format!("{j_src}/01. Liyue.flac"),
                format!("{j}/1-01 - Liyue.flac"),
                format!("{j}/Disc 1/01 - Liyue.flac"),
                track(JADE, "Liyue", 1, &["DISCNUMBER=1/2"]),
            ),
            staged(
                format!("{j_src}/02. Rex Incognito.flac"),
                format!("{j}/1-02 - Rex Incognito.flac"),
                format!("{j}/Disc 1/02 - Rex Incognito.flac"),
                track(JADE, "Rex Incognito", 2, &["DISCNUMBER=1/2"]),
            ),
            staged(
                format!("{j_src}/03. Gilded Runway.flac"),
                format!("{j}/2-01 - Gilded Runway.flac"),
                format!("{j}/Disc 2/01 - Gilded Runway.flac"),
                track(JADE, "Gilded Runway", 1, &["DISCNUMBER=2/2"]),
            ),
            same_in_both(
                format!("{j_src}/Cover.png"),
                format!("{j}/Cover.png"),
                b"png",
            ),
            same_in_both(
                format!("{j_src}/{JADE}.log"),
                format!("{j}/{JADE}.log"),
                b"EAC log, checksum 1234",
            ),
            same_in_both(
                format!("{j_src}/{JADE}.cue"),
                format!("{j}/{JADE}.cue"),
                b"FILE \"01. Liyue.flac\" WAVE",
            ),
            // `DiscNN/` sub-folders, tagged (`DISCNUMBER` + `DISCTOTAL`), MBID,
            // a disc subtitle on disc 1, and one same-named EAC log per disc.
            staged(
                format!("{f_src}/Disc01/1-01 Forest of Jnana and Vidya.flac"),
                format!("{f}/1-01 - Forest of Jnana and Vidya.flac"),
                format!("{f}/Disc 1/01 - Forest of Jnana and Vidya.flac"),
                track(
                    FOREST,
                    "Forest of Jnana and Vidya",
                    1,
                    &["DISCNUMBER=1", "DISCTOTAL=2", sumeru, &mbid],
                ),
            ),
            staged(
                format!("{f_src}/Disc02/2-01 Fading Dawn.flac"),
                format!("{f}/2-01 - Fading Dawn.flac"),
                format!("{f}/Disc 2/01 - Fading Dawn.flac"),
                track(
                    FOREST,
                    "Fading Dawn",
                    1,
                    &["DISCNUMBER=2", "DISCTOTAL=2", &mbid],
                ),
            ),
            // Same name in both disc folders: `Disc N - ` only where they'd collide.
            staged(
                format!("{f_src}/Disc01/Forest.log"),
                format!("{f}/Disc 1 - Forest.log"),
                format!("{f}/Disc 1/Forest.log"),
                b"disc 1 log".to_vec(),
            ),
            staged(
                format!("{f_src}/Disc02/Forest.log"),
                format!("{f}/Disc 2 - Forest.log"),
                format!("{f}/Disc 2/Forest.log"),
                b"disc 2 log!".to_vec(),
            ),
            same_in_both(
                format!("{f_src}/cover.jpg"),
                format!("{f}/cover.jpg"),
                b"jpg",
            ),
            // `CD1/CD2` with NO disc tags: the disc number is inferred from the
            // folder names only (the plan warns the one-folder layout loses it).
            staged(
                format!("{v_src}/CD1/01 Wave.flac"),
                format!("{v}/1-01 - Wave.flac"),
                format!("{v}/Disc 1/01 - Wave.flac"),
                track(VOYAGE, "Wave", 1, &[]),
            ),
            staged(
                format!("{v_src}/CD2/01 Thunder.flac"),
                format!("{v}/2-01 - Thunder.flac"),
                format!("{v}/Disc 2/01 - Thunder.flac"),
                track(VOYAGE, "Thunder", 1, &[]),
            ),
            // Two editions, same ALBUM tag, different folders: distinct albums.
            same_in_both(
                format!("{fp_src}/01. {FOOTPRINTS}.flac"),
                format!("{fp}/01 - {FOOTPRINTS}.flac"),
                &track(FOOTPRINTS, FOOTPRINTS, 1, &[]),
            ),
            same_in_both(
                format!("{fp_src}/cover.jpg"),
                format!("{fp}/cover.jpg"),
                b"jpg-a",
            ),
            same_in_both(
                format!("{fp_src} (Chinese Version)/01. {FOOTPRINTS}.flac"),
                format!("{fp} (2)/01 - {FOOTPRINTS}.flac"),
                &track(FOOTPRINTS, FOOTPRINTS, 1, &[]),
            ),
            same_in_both(
                format!("{fp_src} (Chinese Version)/cover.jpg"),
                format!("{fp} (2)/cover.jpg"),
                b"jpg-b",
            ),
        ]
    }

    /// OS junk in the flat album: never organized, never swept, keeps its folder.
    fn junk() -> String {
        format!("Flac/{ARTIST}/2020.11.06 {JADE}/Thumbs.db")
    }

    /// `p` relative to `root`, `/`-separated.
    fn rel(root: &Path, p: &Path) -> String {
        let parts: Vec<String> = p
            .strip_prefix(root)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        parts.join("/")
    }

    /// Every file under `root` (relative path -> bytes) and every folder.
    fn snapshot(root: &Path) -> (BTreeMap<String, Vec<u8>>, BTreeSet<String>) {
        fn walk(root: &Path, dir: &Path, out: &mut (BTreeMap<String, Vec<u8>>, BTreeSet<String>)) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    out.1.insert(rel(root, &p));
                    walk(root, &p, out);
                } else {
                    out.0.insert(rel(root, &p), std::fs::read(&p).unwrap());
                }
            }
        }
        let mut out = Default::default();
        if root.exists() {
            walk(root, root, &mut out);
        }
        out
    }

    /// Just the files of [`snapshot`].
    fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
        snapshot(root).0
    }

    fn run(strategy: Strategy, template: &str) {
        let disc_folders = template.ends_with(super::DISC_FOLDERS);
        let tag = format!("{strategy:?}_{}", template.replace('+', "_"));
        let (src, dest) = (fresh(&format!("{tag}_src")), fresh(&format!("{tag}_dst")));
        let stage = staging();
        for f in &stage {
            put(&src, &f.src, &f.bytes);
        }
        put(&src, &junk(), b"junk");
        let src_before = snapshot(&src);
        // The exact managed tree, every file byte-identical to its source (the
        // organizer never rewrites: an EAC log's checksum stays valid, tags survive).
        let want: BTreeMap<String, Vec<u8>> = stage
            .iter()
            .map(|f| {
                let dst = if disc_folders { &f.discs } else { &f.one };
                (dst.clone(), f.bytes.clone())
            })
            .collect();
        assert_eq!(
            want.len(),
            17,
            "{tag}: the fixture's destinations are distinct"
        );

        // Plan exactly as the "Add music" worker does, with the picked folder as root.
        let plan = plan_import(&src, &dest, template, strategy);
        let s = summarize(&plan);
        assert_eq!((s.audio, s.side), (9, 8), "{tag}: 9 tracks + 8 side files");
        assert_eq!((s.dup, s.conflict), (0, 0), "{tag}: a fresh destination");
        assert!(
            s.left.is_empty(),
            "{tag}: no album keeps its side files: {:?}",
            s.left
        );
        assert_eq!(
            s.folder_inferred,
            usize::from(!disc_folders),
            "{tag}: only the CD1/CD2 album, and only in the one-folder layout"
        );

        let summary = run_apply(&plan, |_| {});
        assert!(summary.failed.is_empty(), "{tag}: {:?}", summary.failed);
        let after = files(&dest);
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            want.keys().collect::<Vec<_>>(),
            "{tag}: dest tree"
        );
        assert!(
            after == want,
            "{tag}: every file byte-identical to its source"
        );

        match strategy {
            Strategy::Copy => {
                assert_eq!((summary.copied, summary.moved), (17, 0), "{tag}");
                assert_eq!(summary.pruned_dirs, 0, "{tag}: Copy never prunes");
                assert!(
                    snapshot(&src) == src_before,
                    "{tag}: Copy leaves the source as is"
                );
                // Re-run: everything is already there, byte for byte.
                let plan2 = plan_import(&src, &dest, template, strategy);
                let again = summarize(&plan2);
                assert_eq!(
                    (again.copy, again.dup, again.conflict),
                    (0, 17, 0),
                    "{tag}: idempotent plan"
                );
                let s2 = run_apply(&plan2, |_| {});
                assert!(s2.failed.is_empty(), "{tag}: {:?}", s2.failed);
                assert_eq!((s2.copied, s2.skipped, s2.pruned_dirs), (0, 17, 0), "{tag}");
                assert!(files(&dest) == want, "{tag}: the re-run changes nothing");
                assert!(snapshot(&src) == src_before, "{tag}: nor the source");
            }
            Strategy::Move => {
                assert_eq!((summary.copied, summary.moved), (0, 17), "{tag}");
                let (left, left_dirs) = snapshot(&src);
                assert_eq!(
                    left.into_keys().collect::<Vec<_>>(),
                    [junk()],
                    "{tag}: only junk stays behind"
                );
                // Every emptied folder is pruned; the junk keeps its album folder
                // (and so its parents), and the picked folder itself is never removed.
                assert!(src.is_dir(), "{tag}: the picked folder survives");
                assert_eq!(
                    left_dirs,
                    BTreeSet::from([
                        "Flac".to_owned(),
                        format!("Flac/{ARTIST}"),
                        format!("Flac/{ARTIST}/2020.11.06 {JADE}"),
                    ]),
                    "{tag}: pruned down to the junk's folder"
                );
                // Forest/Disc01, Forest/Disc02, Forest, Voyage/CD1, Voyage/CD2,
                // Voyage, Footprints, Footprints (Chinese Version).
                assert_eq!(summary.pruned_dirs, 8, "{tag}: emptied folders pruned");
                // Re-run: nothing left to organize, nothing changes.
                let plan2 = plan_import(&src, &dest, template, strategy);
                assert!(plan2.entries.is_empty(), "{tag}: nothing left to plan");
                let s2 = run_apply(&plan2, |_| {});
                assert!(s2.failed.is_empty(), "{tag}: {:?}", s2.failed);
                assert_eq!((s2.moved, s2.skipped, s2.pruned_dirs), (0, 0, 0), "{tag}");
                assert!(files(&dest) == want, "{tag}: the re-run changes nothing");
                assert_eq!(files(&src).len(), 1, "{tag}: the junk stays");
            }
        }

        // Rescan the managed library the way `organize_confirm` does (watch the
        // destination, fold the update in), then assert the Albums page model.
        let mut st = State::new(PlayerHandle::offline());
        let up = st
            .host
            .add_folder(&dest)
            .expect("watch the managed library");
        crate::apply_watch(&mut st, up);
        assert_eq!(st.library.len(), 9, "{tag}: every track imported");
        let albums = st.grouped_albums();
        let mut names: Vec<&str> = albums.iter().map(|a| a.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [FOOTPRINTS, FOOTPRINTS, FOREST, JADE, VOYAGE],
            "{tag}: no CD-split albums; the two editions stay separate"
        );
        let by = |n: &str| albums.iter().find(|a| a.name == n).unwrap();
        let shape = |n: &str| (by(n).disc_count, by(n).ids.len(), by(n).artist.as_str());
        assert_eq!(shape(JADE), (2, 3, ARTIST), "{tag}: {JADE}");
        assert_eq!(shape(FOREST), (2, 2, ARTIST), "{tag}: {FOREST}");
        // Folder-inferred discs survive only as `Disc N/` folders: the one-folder
        // layout loses them (the organizer never writes tags), which is exactly
        // what the plan's `folder_inferred` warning announced.
        let voyage_discs = if disc_folders { 2 } else { 1 };
        assert_eq!(shape(VOYAGE), (voyage_discs, 2, ARTIST), "{tag}: {VOYAGE}");
        let editions: Vec<_> = albums.iter().filter(|a| a.name == FOOTPRINTS).collect();
        assert_ne!(
            editions[0].key, editions[1].key,
            "{tag}: two album identities"
        );
        assert!(
            editions
                .iter()
                .all(|a| (a.disc_count, a.ids.len()) == (1, 1)),
            "{tag}: each edition is a one-disc, one-track album"
        );

        // The disc sections behind the album page: (disc, track) order, the
        // subtitle read back from the untouched tags.
        let sections = |n: &str| {
            let members: Vec<_> = by(n).ids.iter().filter_map(|id| st.track(*id)).collect();
            soul_player_host::album_tracklist(&members)
                .into_iter()
                .map(|d| (d.disc_no, d.subtitle.map(|s| s.to_string()), d.rows.len()))
                .collect::<Vec<_>>()
        };
        assert_eq!(sections(JADE), [(1, None, 2), (2, None, 1)], "{tag}");
        assert_eq!(
            sections(FOREST),
            [(1, Some("Battles of Sumeru".to_owned()), 1), (2, None, 1)],
            "{tag}"
        );
        let jade_titles: Vec<_> = by(JADE)
            .ids
            .iter()
            .filter_map(|id| st.track(*id)?.title.clone())
            .collect();
        assert_eq!(
            jade_titles,
            ["Liyue", "Rex Incognito", "Gilded Runway"],
            "{tag}"
        );

        // The album page draws the disc headers.
        let forest: AlbumRef = by(FOREST).open_ref();
        st.route = Route::Album;
        st.open_album = Some(forest);
        let env = Env {
            system_dark: true,
            scale: 1.0,
        };
        let mut rt = Runtime::new(st, crate::view, (1200.0, 900.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        let text = |k: &str| {
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some(k))
                .and_then(|e| e.text)
        };
        assert_eq!(
            text("album-disc-1-title").as_deref(),
            Some("Disc 1 · Battles of Sumeru"),
            "{tag}"
        );
        assert_eq!(
            text("album-disc-2-title").as_deref(),
            Some("Disc 2"),
            "{tag}"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn organize_copy_one_folder_then_rescan() {
        let _locale = crate::config::use_test_locale("en");
        let _cfg = crate::config::use_test_config_dir();
        run(Strategy::Copy, "artist-album");
    }

    #[test]
    fn organize_copy_disc_folders_then_rescan() {
        let _locale = crate::config::use_test_locale("en");
        let _cfg = crate::config::use_test_config_dir();
        run(Strategy::Copy, "artist-album+disc-folders");
    }

    #[test]
    fn organize_move_one_folder_then_rescan() {
        let _locale = crate::config::use_test_locale("en");
        let _cfg = crate::config::use_test_config_dir();
        run(Strategy::Move, "artist-album");
    }

    #[test]
    fn organize_move_disc_folders_then_rescan() {
        let _locale = crate::config::use_test_locale("en");
        let _cfg = crate::config::use_test_config_dir();
        run(Strategy::Move, "artist-album+disc-folders");
    }
}
