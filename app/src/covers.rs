//! The on-disk custom-cover store: user-uploaded artwork for an ARTIST or an
//! ALBUM, kept as one flat file per entity under `<config>/covers` and shown
//! everywhere that artist/album's art appears (grid cards + detail heroes).
//!
//! Layout:
//! ```text
//! <config>/covers/
//!   artist-<slug>.<ext>   (a custom artist avatar)
//!   album-<slug>.<ext>    (a custom album cover)
//! ```
//!
//! The file STEM (`artist-<slug>` / `album-<slug>`) is the identity: a cover is
//! keyed by the artist/album *name* slugified, so it survives library rescans
//! (which re-mint volatile track ids). Mirrors the proven playlist custom-cover
//! feature (`playlists::store::set_custom_cover`): same accepted extensions,
//! same 16 MiB cap, same remove-old-then-copy replace, same path-resolves-to-
//! `None`-when-the-file-vanished semantics.
#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config;

/// Accepted custom-cover image extensions, lowercased and without the dot.
/// Matches `playlists::store::CUSTOM_COVER_EXTENSIONS`.
const COVER_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

/// Maximum accepted custom-cover source file size: 16 MiB (matches the
/// playlist custom-cover cap).
const COVER_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The loaded custom-cover store: the `<config>/covers` directory plus the set
/// of stored file stems (`artist-<slug>` / `album-<slug>`), so a path lookup
/// can skip the directory scan entirely for an entity that was never given a
/// custom cover (the overwhelmingly common case, a lookup runs per card, per
/// frame).
pub(crate) struct CoverStore {
    dir: PathBuf,
    known: HashSet<String>,
}

/// The `<config>/covers` directory. Falls back to `./covers` only if the
/// config dir can't be resolved (never in practice), mirroring
/// `playlists::store::playlist_store_path`'s own fallback.
fn covers_dir() -> PathBuf {
    config::soul_player_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("covers")
}

/// Slugify `name` for use in a cover file stem: lowercase, every non
/// ascii-alphanumeric char becomes `-`, consecutive `-` collapse, and leading/
/// trailing `-` are trimmed. Two names that slugify the same share a stem (an
/// accepted collision risk, as with playlist slugs).
fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// The file stem a given entity's cover is stored under.
fn artist_stem(name: &str) -> String {
    format!("artist-{}", slug(name))
}
fn album_stem(name: &str) -> String {
    format!("album-{}", slug(name))
}

/// The path of the file in `dir` whose stem is exactly `stem`, if one exists
/// on disk right now (any accepted extension). `None` if the file is gone.
fn find_by_stem(dir: &Path, stem: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_stem().and_then(|s| s.to_str()) == Some(stem) {
            return Some(path);
        }
    }
    None
}

/// Remove every file in `dir` whose stem is exactly `stem` (best-effort;
/// ignores I/O errors). Called before writing a fresh cover so a previous
/// cover saved under a different extension is never orphaned.
fn remove_by_stem(dir: &Path, stem: &str) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_stem().and_then(|s| s.to_str()) == Some(stem) {
            let _ = fs::remove_file(&path);
        }
    }
}

impl CoverStore {
    /// Load the store: ensure `<config>/covers` exists, then scan it and record
    /// every file's stem. Best-effort: a missing/unreadable directory just
    /// yields an empty `known` set (nothing has a custom cover yet). Never
    /// panics.
    pub(crate) fn load() -> Self {
        let dir = covers_dir();
        let _ = fs::create_dir_all(&dir);
        let mut known = HashSet::new();
        if let Ok(read) = fs::read_dir(&dir) {
            for entry in read.flatten() {
                let path = entry.path();
                if path.is_file()
                    && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
                {
                    known.insert(stem.to_string());
                }
            }
        }
        Self { dir, known }
    }

    /// Validate `src` (an accepted image extension, lowercased, a real file at
    /// most [`COVER_MAX_BYTES`]) and copy it into the store as `<stem>.<ext>`,
    /// removing any prior `<stem>.*` first so a cover replaced under a
    /// different extension doesn't orphan the old file. Returns `true` iff
    /// applied; never panics (an invalid `src` or any I/O failure yields
    /// `false`). Shared by [`Self::set_artist_cover`]/[`Self::set_album_cover`].
    fn set_cover(&mut self, stem: String, src: &Path) -> bool {
        let Some(ext) = src
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
        else {
            return false;
        };
        if !COVER_EXTS.contains(&ext.as_str()) {
            return false;
        }
        let Ok(metadata) = fs::metadata(src) else {
            return false;
        };
        if !metadata.is_file() || metadata.len() > COVER_MAX_BYTES {
            return false;
        }
        if fs::create_dir_all(&self.dir).is_err() {
            return false;
        }
        remove_by_stem(&self.dir, &stem);
        if fs::copy(src, self.dir.join(format!("{stem}.{ext}"))).is_err() {
            return false;
        }
        self.known.insert(stem);
        true
    }

    /// Set artist `name`'s custom avatar from the image file `src`.
    pub(crate) fn set_artist_cover(&mut self, name: &str, src: &Path) -> bool {
        self.set_cover(artist_stem(name), src)
    }

    /// Set album `name`'s custom cover from the image file `src`.
    pub(crate) fn set_album_cover(&mut self, name: &str, src: &Path) -> bool {
        self.set_cover(album_stem(name), src)
    }

    /// The absolute path to the stored cover for `stem`, if one was recorded
    /// AND the file still exists on disk (a set-but-missing file, e.g. deleted
    /// out-of-band, resolves to `None`).
    fn cover_path(&self, stem: &str) -> Option<PathBuf> {
        if !self.known.contains(stem) {
            return None;
        }
        find_by_stem(&self.dir, stem)
    }

    /// Artist `name`'s custom avatar path, or `None` if unset/missing.
    pub(crate) fn artist_cover_path(&self, name: &str) -> Option<PathBuf> {
        self.cover_path(&artist_stem(name))
    }

    /// Album `name`'s custom cover path, or `None` if unset/missing.
    pub(crate) fn album_cover_path(&self, name: &str) -> Option<PathBuf> {
        self.cover_path(&album_stem(name))
    }

    /// Delete the stored cover for `stem` (best-effort) and forget it.
    fn clear_cover(&mut self, stem: &str) {
        remove_by_stem(&self.dir, stem);
        self.known.remove(stem);
    }

    /// Clear artist `name`'s custom avatar.
    pub(crate) fn clear_artist_cover(&mut self, name: &str) {
        self.clear_cover(&artist_stem(name));
    }

    /// Clear album `name`'s custom cover.
    pub(crate) fn clear_album_cover(&mut self, name: &str) {
        self.clear_cover(&album_stem(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp directory unique to this call (process id + atomic counter), so
    /// parallel tests never collide. Holds only the *source* image a test
    /// copies from; never the cover store itself (that lives under the shared
    /// test config dir). Mirrors `playlists::store`'s own helper.
    fn temp_source_dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "soul_rune_cover_src_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a tiny but real PNG (just the 8-byte file signature; it never
    /// needs to decode, only to exist as a `.png` under the size cap) to a
    /// fresh temp dir and return its path.
    fn write_temp_png() -> PathBuf {
        const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let path = temp_source_dir().join("source.png");
        fs::write(&path, PNG_SIGNATURE).unwrap();
        path
    }

    fn write_temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path = temp_source_dir().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    /// Start from an empty store on disk. The per-process test config dir
    /// persists for the whole binary run, so a cover file written by an earlier
    /// test would otherwise still be sitting there (same reset the playlist
    /// store tests do).
    fn reset() {
        let _ = fs::remove_dir_all(covers_dir());
    }

    #[test]
    fn slug_normalizes_names() {
        assert_eq!(slug("Broke For Free"), "broke-for-free");
        assert_eq!(slug("  A!!!B  "), "a-b");
        assert_eq!(slug("AC/DC"), "ac-dc");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn set_album_cover_then_path_resolves_and_clear_removes_it() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = CoverStore::load();
        assert!(
            s.album_cover_path("Lantern 19").is_none(),
            "nothing set yet"
        );

        let img = write_temp_png();
        assert!(s.set_album_cover("Lantern 19", &img), "a PNG applies");

        // Fresh load from disk also sees it (the stem was persisted as a file).
        let s2 = CoverStore::load();
        let path = s2
            .album_cover_path("Lantern 19")
            .expect("album cover path resolves after a reload");
        assert!(path.exists());
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("album-")
        );

        s.clear_album_cover("Lantern 19");
        assert!(
            s.album_cover_path("Lantern 19").is_none(),
            "clear removes the cover"
        );
        assert!(!path.exists(), "and deletes the file");
    }

    #[test]
    fn set_artist_cover_then_path_resolves_and_clear_removes_it() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = CoverStore::load();
        assert!(s.artist_cover_path("Broke For Free").is_none());

        assert!(s.set_artist_cover("Broke For Free", &write_temp_png()));
        let path = s
            .artist_cover_path("Broke For Free")
            .expect("artist cover path resolves");
        assert!(path.exists());
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("artist-")
        );

        s.clear_artist_cover("Broke For Free");
        assert!(s.artist_cover_path("Broke For Free").is_none());
        assert!(!path.exists());
    }

    #[test]
    fn rejects_non_image_and_leaves_no_cover() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = CoverStore::load();
        assert!(!s.set_album_cover("X", &write_temp_file("x.txt", b"nope")));
        assert!(s.album_cover_path("X").is_none());
    }

    #[test]
    fn replacing_a_cover_removes_the_old_extension() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = CoverStore::load();
        // First a .png, then replace with a .jpg: only the .jpg must remain
        // (the stem's old .png is removed), so exactly one file carries the stem.
        assert!(s.set_album_cover("Mix", &write_temp_png()));
        assert!(s.set_album_cover(
            "Mix",
            &write_temp_file("source.jpg", b"\xff\xd8\xff\xe0jpg")
        ));
        let path = s.album_cover_path("Mix").expect("cover still set");
        assert_eq!(path.extension().unwrap(), "jpg");
        let same_stem = fs::read_dir(covers_dir())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.path()
                    .file_stem()
                    .and_then(|st| st.to_str())
                    .is_some_and(|st| st == "album-mix")
            })
            .count();
        assert_eq!(
            same_stem, 1,
            "the replaced .png is gone, only the .jpg remains"
        );
    }

    #[test]
    fn a_set_but_deleted_file_resolves_to_none() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = CoverStore::load();
        assert!(s.set_album_cover("Gone", &write_temp_png()));
        let path = s.album_cover_path("Gone").unwrap();
        fs::remove_file(&path).unwrap(); // deleted out-of-band
        assert!(
            s.album_cover_path("Gone").is_none(),
            "a vanished file resolves to None even while the stem is still known"
        );
    }
}
