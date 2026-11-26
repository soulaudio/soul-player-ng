//! Opening audio files handed to the app: today the audio files among the launch arguments
//! (`soul-player song.flac`); Finder "Open With" / double-click / Dock drop once Rune
//! exposes the macOS open-documents event (anomalyco/rune#157), the app won't carry the
//! AppKit `unsafe` itself.
//!
//! Library-centric like Apple Music: a file already in the library plays straight away; a
//! file outside it has its **folder added as a source** (exactly what Settings → Add folder
//! does) and plays once that scan lands.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use soul_player_host::Command;

use crate::State;

/// Extensions the player decodes (soul-audio-core + the desktop container dispatch). No
/// `.aac`: AAC decode is deliberately off (patent licensing); `.m4a` stays for ALAC.
const AUDIO_EXTS: &[&str] = &[
    "flac", "wav", "wave", "aif", "aiff", "aifc", "caf", "mp3", "mp1", "mp2", "m4a", "mp4", "alac",
    "ogg", "oga", "opus", "wv", "dsf", "dff", "mka", "webm",
];

pub(crate) fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The audio files among the process's launch arguments (non-files and non-audio skipped,
/// so flags and stray arguments are ignored).
pub(crate) fn from_args(args: impl IntoIterator<Item = String>) -> Vec<PathBuf> {
    args.into_iter()
        .map(PathBuf::from)
        .filter(|p| p.is_file() && is_audio(p))
        .collect()
}

/// What to do with a set of opened files given which are already in the library.
#[derive(Debug, PartialEq)]
pub(crate) struct Plan {
    /// Library ids to play, in the order the files were opened.
    pub play: Vec<u32>,
    /// Folders to add as sources (deduplicated, first-seen order) for files not yet in the
    /// library.
    pub add_folders: Vec<PathBuf>,
}

pub(crate) fn plan(paths: &[PathBuf], ids: &[Option<u32>]) -> Plan {
    let mut play = Vec::new();
    let mut add_folders: Vec<PathBuf> = Vec::new();
    for (p, id) in paths.iter().zip(ids) {
        match id {
            Some(id) => play.push(*id),
            None => {
                if let Some(dir) = p.parent()
                    && !add_folders.iter().any(|d| d == dir)
                {
                    add_folders.push(dir.to_path_buf());
                }
            }
        }
    }
    Plan { play, add_folders }
}

/// Open `paths` (any mix of files; non-audio ones are ignored).
pub(crate) fn open(s: &mut State, paths: Vec<PathBuf>) {
    let paths: Vec<PathBuf> = paths.into_iter().filter(|p| is_audio(p)).collect();
    if paths.is_empty() {
        return;
    }
    tracing::debug!(
        startup_scan_pending = s.startup_scan_pending,
        queued = s.pending_open.len(),
        library = s.library.len(),
        "open request"
    );
    if s.startup_scan_pending || !s.pending_open.is_empty() {
        // A scan is about to replace the library (track ids would go stale): queue these
        // behind it; `apply_startup_scan` / `after_scan` open them when it lands.
        s.pending_open.extend(paths);
        return;
    }
    let ids = s.host.track_ids_for_paths(&paths);
    let p = plan(&paths, &ids);
    tracing::info!(
        files = paths.len(),
        in_library = p.play.len(),
        folders_to_add = p.add_folders.len(),
        "opening files"
    );
    if p.add_folders.is_empty() {
        play(s, p.play);
        return;
    }
    // Some files aren't in the library yet: add their folders (one background scan), then
    // resolve again and play everything that was opened.
    s.pending_open = paths;
    let folders = p.add_folders;
    crate::scan_in_background(
        s,
        move |h| {
            let mut last = None;
            for f in &folders {
                last = Some(h.add_folder(f)?);
            }
            last.ok_or_else(|| anyhow::anyhow!("no folder to add"))
        },
        after_scan,
        after_scan_failed,
    );
}

/// Scan landed: persist the new sources like any Sources edit, then play what was opened.
fn after_scan(s: &mut State, up: soul_player_host::WatchUpdate) {
    crate::apply_and_save(s, up);
    let paths = std::mem::take(&mut s.pending_open);
    let ids: Vec<u32> = s
        .host
        .track_ids_for_paths(&paths)
        .into_iter()
        .flatten()
        .collect();
    if ids.len() < paths.len() {
        tracing::warn!(
            opened = paths.len(),
            found = ids.len(),
            "some opened files did not appear in the library after adding their folders"
        );
    }
    play(s, ids);
}

/// The folder-add scan behind an open request failed. Drain the queued paths (so a
/// dropped Err does not leave `pending_open` stuck, which would wedge every later
/// open behind the `open`-guard), log it, then play whatever already resolves against
/// the current library rather than losing the request silently.
fn after_scan_failed(s: &mut State, e: anyhow::Error) {
    let paths = std::mem::take(&mut s.pending_open);
    tracing::error!(
        error = %e,
        queued = paths.len(),
        "adding folders for an open request failed"
    );
    let ids: Vec<u32> = s
        .host
        .track_ids_for_paths(&paths)
        .into_iter()
        .flatten()
        .collect();
    play(s, ids);
}

fn play(s: &mut State, ids: Vec<u32>) {
    if !ids.is_empty() {
        s.host.send(Command::PlayContext { ids, start: 0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_every_format_the_player_decodes_case_insensitively() {
        for f in [
            "a.FLAC", "b.dsf", "c.DFF", "d.m4a", "e.opus", "f.wv", "g.aiff",
        ] {
            assert!(is_audio(Path::new(f)), "{f}");
        }
        for f in ["cover.jpg", "notes.txt", "noext", "playlist.m3u"] {
            assert!(!is_audio(Path::new(f)), "{f}");
        }
    }

    #[test]
    fn plays_library_files_in_order_and_adds_each_missing_folder_once() {
        let paths: Vec<PathBuf> = [
            "/lib/a.flac",
            "/new/x.flac",
            "/lib/b.flac",
            "/new/y.flac",
            "/other/z.dsf",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let ids = [Some(7), None, Some(3), None, None];
        assert_eq!(
            plan(&paths, &ids),
            Plan {
                play: vec![7, 3],
                add_folders: vec![PathBuf::from("/new"), PathBuf::from("/other")],
            }
        );
    }

    #[test]
    fn launch_arguments_keep_only_existing_audio_files() {
        let dir = std::env::temp_dir().join(format!("soul-open-args-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("song.flac");
        std::fs::write(&song, b"x").unwrap();
        std::fs::write(dir.join("cover.jpg"), b"x").unwrap();
        let args = [
            "--snap".to_string(),
            song.display().to_string(),
            dir.join("cover.jpg").display().to_string(),
            dir.join("gone.flac").display().to_string(),
        ];
        assert_eq!(from_args(args), vec![song]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End to end through the real host: a file already in the library plays at once; a file
    /// in a new folder gets that folder added and then plays.
    #[test]
    fn opening_files_plays_them_adding_unknown_folders_first() {
        let _cfg = crate::config::use_test_config_dir();
        let lib = std::env::temp_dir().join(format!("soul-open-lib-{}", std::process::id()));
        let new = std::env::temp_dir().join(format!("soul-open-new-{}", std::process::id()));
        for d in [&lib, &new] {
            std::fs::create_dir_all(d).unwrap();
        }
        let td = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdata");
        std::fs::copy(td.join("meta.flac"), lib.join("in-library.flac")).unwrap();
        std::fs::copy(td.join("meta.wav"), new.join("not-yet.wav")).unwrap();

        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        let up = s.host.add_folder(&lib).unwrap();
        crate::apply_watch(&mut s, up);
        assert_eq!(s.library.len(), 1);

        open(
            &mut s,
            vec![new.join("not-yet.wav"), lib.join("in-library.flac")],
        );
        // Headless (no app handle): the scan ran synchronously and the folder was added.
        assert!(
            s.sources.iter().any(|x| x.path == new),
            "the unknown file's folder became a source"
        );
        assert_eq!(s.library.len(), 2, "both files are in the library now");
        assert!(s.pending_open.is_empty(), "the pending open was consumed");
        let _ = std::fs::remove_dir_all(&lib);
        let _ = std::fs::remove_dir_all(&new);
    }

    #[test]
    fn files_opened_during_the_startup_scan_wait_for_it() {
        let _cfg = crate::config::use_test_config_dir();
        let lib = std::env::temp_dir().join(format!("soul-open-startup-{}", std::process::id()));
        std::fs::create_dir_all(&lib).unwrap();
        let td = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../testdata");
        std::fs::copy(td.join("meta.flac"), lib.join("song.flac")).unwrap();

        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.host.set_folders(std::slice::from_ref(&lib)).unwrap();
        s.startup_scan_pending = true;
        open(&mut s, vec![lib.join("song.flac")]);
        assert_eq!(
            s.pending_open,
            vec![lib.join("song.flac")],
            "held, not resolved yet"
        );

        let up = s.host.rescan_sources().unwrap();
        crate::apply_startup_scan(&mut s, up);
        assert!(!s.startup_scan_pending);
        assert!(s.pending_open.is_empty(), "opened once the scan landed");
        let _ = std::fs::remove_dir_all(&lib);
    }
}
