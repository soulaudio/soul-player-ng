//! The on-disk custom-cover store: user-uploaded artwork for an ARTIST or an
//! ALBUM, kept as one flat file per entity under `<config>/covers` and shown
//! everywhere that artist/album's art appears (grid cards + detail heroes).
//!
//! Layout:
//! ```text
//! <config>/covers/
//!   artist-<slug>.<ext>       (a custom artist avatar)
//!   albumid-<fnv64>.<ext>     (a custom album cover, keyed by album identity)
//!   legacy-title-keyed/       (retired pre-identity album-<slug>.<ext> files)
//! ```
//!
//! The file STEM is the identity, so a cover survives library rescans (which
//! re-mint volatile track ids). An artist is keyed by its name slugified
//! (`artist-<slug>`). An album is keyed by its [`AlbumIdentity`] (album root
//! folder + title) hashed with FNV-1a 64 using the specification's constants
//! (`albumid-<16 hex>`), so two same-title editions in two folders keep
//! separate covers and the stem never changes across Rust versions. Mirrors the proven playlist custom-cover
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

/// Where retired title-keyed album covers are moved (kept, never deleted).
/// A sub-folder, so [`CoverStore::load`]'s flat scan never re-reads them.
const LEGACY_BACKUP_DIR: &str = "legacy-title-keyed";

/// An album's durable identity for its custom cover:
/// [`soul_player_host::album_identity_key`] of a member track's locator and the
/// album title. Stable across rescans (track and album ids are not) and
/// distinct for two same-title editions in two folders.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AlbumIdentity(String);

impl AlbumIdentity {
    pub(crate) fn new(key: String) -> Self {
        Self(key)
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// What [`CoverStore::adopt_legacy_album_covers`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LegacyCoverAdoption {
    /// Identity covers written from a legacy file.
    pub(crate) adopted: usize,
    /// Legacy files moved to [`LEGACY_BACKUP_DIR`] after all their adoptions succeeded.
    pub(crate) retired: usize,
    /// Legacy files left in place (no current album, or an adoption failed).
    pub(crate) kept: usize,
}

/// FNV-1a 64 over `s`'s UTF-8 bytes (offset basis 0xcbf29ce484222325, prime
/// 0x100000001b3). Fixed by specification, so cover file names stay stable.
fn fnv1a64(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The file stem an album's identity-keyed cover is stored under.
fn album_identity_stem(id: &AlbumIdentity) -> String {
    format!("albumid-{:016x}", fnv1a64(id.as_str()))
}

/// Whether `stem` is a pre-identity, title-keyed album cover (`album-<slug>`).
fn is_legacy_album_stem(stem: &str) -> bool {
    stem.starts_with("album-")
}

/// The loaded custom-cover store: the `<config>/covers` directory plus the set
/// of stored file stems (`artist-<slug>` / `albumid-<fnv64>`, plus any legacy
/// `album-<slug>` awaiting adoption), so a path lookup
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

/// The file stem an artist's avatar is stored under.
fn artist_stem(name: &str) -> String {
    format!("artist-{}", slug(name))
}

/// The legacy title-keyed album stem (pre-identity). Used only to match legacy
/// files during migration.
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

    /// Set the album's custom cover from the image file `src`.
    pub(crate) fn set_album_cover(&mut self, id: &AlbumIdentity, src: &Path) -> bool {
        self.set_cover(album_identity_stem(id), src)
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

    /// The album's custom cover path, or `None` if unset/missing.
    pub(crate) fn album_cover_path(&self, id: &AlbumIdentity) -> Option<PathBuf> {
        self.cover_path(&album_identity_stem(id))
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

    /// Clear the album's custom cover.
    pub(crate) fn clear_album_cover(&mut self, id: &AlbumIdentity) {
        self.clear_cover(&album_identity_stem(id));
    }

    /// Whether any pre-identity, title-keyed album cover is still in the store.
    /// Cheap (a scan of the known stems), so callers can check it on every
    /// library change.
    pub(crate) fn has_legacy_album_covers(&self) -> bool {
        self.known.iter().any(|s| is_legacy_album_stem(s))
    }

    /// Adopt legacy title-keyed album covers for `albums` (display title,
    /// identity), preserving what each album showed before identity keys: a
    /// legacy `album-<slug>` cover is copied to EVERY listed album whose title
    /// slugifies to that stem, unless that album already has an identity cover
    /// (never overwritten). Only when every copy for a stem succeeded is the
    /// legacy file moved to [`LEGACY_BACKUP_DIR`] (kept, not deleted). A legacy
    /// file matching no listed album stays for a later library. Idempotent.
    pub(crate) fn adopt_legacy_album_covers<'a>(
        &mut self,
        albums: impl IntoIterator<Item = (&'a str, &'a AlbumIdentity)>,
    ) -> LegacyCoverAdoption {
        let mut out = LegacyCoverAdoption::default();
        let legacy: Vec<String> = self
            .known
            .iter()
            .filter(|s| is_legacy_album_stem(s))
            .cloned()
            .collect();
        if legacy.is_empty() {
            return out;
        }
        let mut by_stem: std::collections::HashMap<String, Vec<&AlbumIdentity>> =
            std::collections::HashMap::new();
        for (title, id) in albums {
            let ids = by_stem.entry(album_stem(title)).or_default();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        for stem in legacy {
            let Some(src) = find_by_stem(&self.dir, &stem) else {
                self.known.remove(&stem); // vanished out-of-band
                continue;
            };
            let Some(ids) = by_stem.get(&stem) else {
                out.kept += 1;
                continue;
            };
            let ext = src
                .extension()
                .and_then(|e| e.to_str())
                .map_or_else(|| "png".to_string(), str::to_lowercase);
            let mut all_ok = true;
            for id in ids {
                let dst_stem = album_identity_stem(id);
                if self.known.contains(&dst_stem) {
                    continue;
                }
                match fs::copy(&src, self.dir.join(format!("{dst_stem}.{ext}"))) {
                    Ok(_) => {
                        self.known.insert(dst_stem);
                        out.adopted += 1;
                    }
                    Err(err) => {
                        tracing::warn!(%stem, error = %err, "could not adopt a title-keyed album cover");
                        all_ok = false;
                    }
                }
            }
            let backup = self.dir.join(LEGACY_BACKUP_DIR);
            let retired = all_ok
                && fs::create_dir_all(&backup).is_ok()
                && src
                    .file_name()
                    .is_some_and(|name| fs::rename(&src, backup.join(name)).is_ok());
            if retired {
                self.known.remove(&stem);
                out.retired += 1;
            } else {
                out.kept += 1;
            }
        }
        out
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

    #[test]
    fn slug_normalizes_names() {
        assert_eq!(slug("Broke For Free"), "broke-for-free");
        assert_eq!(slug("  A!!!B  "), "a-b");
        assert_eq!(slug("AC/DC"), "ac-dc");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn set_album_cover_then_path_resolves_and_clear_removes_it() {
        let _iso = config::use_isolated_test_config_dir();
        let a = id("Lantern 19");
        let mut s = CoverStore::load();
        assert!(s.album_cover_path(&a).is_none(), "nothing set yet");

        let img = write_temp_png();
        assert!(s.set_album_cover(&a, &img), "a PNG applies");

        // Fresh load from disk also sees it (the stem was persisted as a file).
        let s2 = CoverStore::load();
        let path = s2
            .album_cover_path(&a)
            .expect("album cover path resolves after a reload");
        assert!(path.exists());
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("albumid-")
        );

        s.clear_album_cover(&a);
        assert!(s.album_cover_path(&a).is_none(), "clear removes the cover");
        assert!(!path.exists(), "and deletes the file");
    }

    #[test]
    fn set_artist_cover_then_path_resolves_and_clear_removes_it() {
        let _iso = config::use_isolated_test_config_dir();
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
        let _iso = config::use_isolated_test_config_dir();
        let mut s = CoverStore::load();
        assert!(!s.set_album_cover(&id("X"), &write_temp_file("x.txt", b"nope")));
        assert!(s.album_cover_path(&id("X")).is_none());
    }

    #[test]
    fn replacing_a_cover_removes_the_old_extension() {
        let _iso = config::use_isolated_test_config_dir();
        let mix = id("Mix");
        let mut s = CoverStore::load();
        // First a .png, then replace with a .jpg: only the .jpg must remain
        // (the stem's old .png is removed), so exactly one file carries the stem.
        assert!(s.set_album_cover(&mix, &write_temp_png()));
        assert!(s.set_album_cover(&mix, &write_temp_file("source.jpg", b"\xff\xd8\xff\xe0jpg")));
        let path = s.album_cover_path(&mix).expect("cover still set");
        assert_eq!(path.extension().unwrap(), "jpg");
        let stem = album_identity_stem(&mix);
        let same_stem = fs::read_dir(covers_dir())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.path()
                    .file_stem()
                    .and_then(|st| st.to_str())
                    .is_some_and(|st| st == stem)
            })
            .count();
        assert_eq!(
            same_stem, 1,
            "the replaced .png is gone, only the .jpg remains"
        );
    }

    #[test]
    fn a_set_but_deleted_file_resolves_to_none() {
        let _iso = config::use_isolated_test_config_dir();
        let gone = id("Gone");
        let mut s = CoverStore::load();
        assert!(s.set_album_cover(&gone, &write_temp_png()));
        let path = s.album_cover_path(&gone).unwrap();
        fs::remove_file(&path).unwrap(); // deleted out-of-band
        assert!(
            s.album_cover_path(&gone).is_none(),
            "a vanished file resolves to None even while the stem is still known"
        );
    }

    const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

    fn id(s: &str) -> AlbumIdentity {
        AlbumIdentity::new(s.to_string())
    }

    /// A legacy title-keyed cover file, as the pre-identity store wrote it.
    fn legacy_cover(title: &str) -> PathBuf {
        let dir = covers_dir();
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{}.png", album_stem(title)));
        fs::write(&p, PNG_SIG).unwrap();
        p
    }

    #[test]
    fn fnv1a64_matches_the_reference_vectors() {
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn album_covers_are_keyed_by_identity_not_title() {
        let _iso = config::use_isolated_test_config_dir();
        let mut s = CoverStore::load();
        let a = id("\u{1f}d:/music/footprints\u{1f}Footprints of the Traveler");
        let b = id("\u{1f}d:/music/footprints cn\u{1f}Footprints of the Traveler");
        assert!(s.set_album_cover(&a, &write_temp_png()));
        let path = s.album_cover_path(&a).expect("set");
        assert!(
            s.album_cover_path(&b).is_none(),
            "a same-title edition keeps its own art"
        );
        assert_eq!(
            path.file_stem().unwrap().to_string_lossy(),
            format!("albumid-{:016x}", fnv1a64(a.as_str()))
        );
        assert!(
            CoverStore::load().album_cover_path(&a).is_some(),
            "survives a reload"
        );
        s.clear_album_cover(&a);
        assert!(s.album_cover_path(&a).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn a_title_keyed_cover_is_adopted_by_every_album_of_that_title_then_retired() {
        let _iso = config::use_isolated_test_config_dir();
        let legacy = legacy_cover("Footprints of the Traveler");
        let mut s = CoverStore::load();
        assert!(s.has_legacy_album_covers());
        let (a, b, other) = (id("a"), id("b"), id("c"));
        let out = s.adopt_legacy_album_covers([
            ("Footprints of the Traveler", &a),
            ("Footprints of the Traveler", &b),
            ("Other", &other),
        ]);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 2,
                retired: 1,
                kept: 0
            }
        );
        assert_eq!(fs::read(s.album_cover_path(&a).unwrap()).unwrap(), PNG_SIG);
        assert!(s.album_cover_path(&b).is_some());
        assert!(s.album_cover_path(&other).is_none());
        assert!(!legacy.exists(), "the title-keyed file is retired");
        assert!(
            covers_dir()
                .join(LEGACY_BACKUP_DIR)
                .join(legacy.file_name().unwrap())
                .exists(),
            "kept as a backup, not deleted"
        );
        assert!(!s.has_legacy_album_covers());
        // Idempotent: a second pass and a reload change nothing.
        assert_eq!(
            s.adopt_legacy_album_covers([("Footprints of the Traveler", &a)]),
            LegacyCoverAdoption::default()
        );
        let reloaded = CoverStore::load();
        assert!(!reloaded.has_legacy_album_covers());
        assert!(reloaded.album_cover_path(&a).is_some());
    }

    #[test]
    fn a_title_keyed_cover_with_no_current_album_waits_for_one() {
        let _iso = config::use_isolated_test_config_dir();
        let legacy = legacy_cover("Not Yet Scanned");
        let mut s = CoverStore::load();
        let out = s.adopt_legacy_album_covers([("Something Else", &id("x"))]);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 0,
                retired: 0,
                kept: 1
            }
        );
        assert!(legacy.exists());
        assert!(s.has_legacy_album_covers());
    }

    #[test]
    fn a_failed_adoption_keeps_the_title_keyed_cover_until_a_retry_succeeds() {
        let _iso = config::use_isolated_test_config_dir();
        let legacy = legacy_cover("Box");
        let (a, b) = (id("a"), id("b"));
        // Block b's destination: a directory where its cover file would go.
        let blocker = covers_dir().join(format!("{}.png", album_identity_stem(&b)));
        fs::create_dir_all(&blocker).unwrap();
        let mut s = CoverStore::load();
        let out = s.adopt_legacy_album_covers([("Box", &a), ("Box", &b)]);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 1,
                retired: 0,
                kept: 1
            }
        );
        assert!(legacy.exists(), "not retired while an adoption failed");
        fs::remove_dir(&blocker).unwrap();
        let out = s.adopt_legacy_album_covers([("Box", &a), ("Box", &b)]);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 1,
                retired: 1,
                kept: 0
            },
            "a is not re-copied"
        );
        assert!(!legacy.exists());
        assert!(s.album_cover_path(&b).is_some());
    }

    #[test]
    fn an_identity_cover_is_never_overwritten_by_a_legacy_one() {
        let _iso = config::use_isolated_test_config_dir();
        let a = id("a");
        let mut s = CoverStore::load();
        assert!(s.set_album_cover(&a, &write_temp_file("mine.jpg", b"\xff\xd8\xff\xe0jpg")));
        legacy_cover("Box");
        let mut s = CoverStore::load();
        let out = s.adopt_legacy_album_covers([("Box", &a)]);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 0,
                retired: 1,
                kept: 0
            }
        );
        assert_eq!(s.album_cover_path(&a).unwrap().extension().unwrap(), "jpg");
    }
}
