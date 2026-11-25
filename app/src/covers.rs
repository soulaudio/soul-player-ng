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

/// The adoption ledger inside [`LEGACY_BACKUP_DIR`]: one `legacy-stem<TAB>albumid-stem`
/// line per cover adopted from a legacy file, so a cover the user cleared is not
/// copied back while that legacy file still waits to be retired.
const ADOPTION_LEDGER: &str = "adopted.tsv";

/// The extension of an in-flight copy. A copy is written as `<name>.partial` and
/// renamed into place only when complete, so a crash never leaves a truncated
/// file under a cover stem; [`CoverStore::load`] discards leftovers.
const PARTIAL_EXT: &str = "partial";

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
    /// Whether this identity names an album root (or server). A title-only
    /// fallback ([`soul_player_host::unlocated_album_identity_key`]) is shared by
    /// every same-title album, so no cover is ever stored under one.
    pub(crate) fn is_located(&self) -> bool {
        !soul_player_host::is_unlocated_album_identity_key(&self.0)
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

/// Copy `src` to `dst` crash-safely: into `<dst>.partial` first, then renamed
/// over `dst`, so `dst` only ever exists complete. The partial file is removed
/// on failure.
fn copy_into_place(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut partial = dst.as_os_str().to_owned();
    partial.push(".");
    partial.push(PARTIAL_EXT);
    let partial = PathBuf::from(partial);
    let res = fs::copy(src, &partial).and_then(|_| fs::rename(&partial, dst));
    if res.is_err() {
        let _ = fs::remove_file(&partial);
    }
    res
}

/// A free path for `name` in `dir`: `name` itself, else `stem (2).ext`,
/// `stem (3).ext`, ..., so an earlier backup is never overwritten.
fn free_backup_path(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().unwrap_or(name).to_string_lossy();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2u32..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .expect("an unused backup name")
}

/// The `(legacy stem, identity stem)` pairs already adopted (see
/// [`ADOPTION_LEDGER`]). Empty when the ledger is missing or unreadable.
fn read_ledger(dir: &Path) -> HashSet<(String, String)> {
    fs::read_to_string(dir.join(LEGACY_BACKUP_DIR).join(ADOPTION_LEDGER))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

/// Append adopted `(legacy stem, identity stem)` pairs to the ledger as one
/// write of complete lines. If the file's last line was torn (a crash mid-write
/// left no trailing newline), the batch starts on a fresh line so the torn
/// fragment can't swallow its first entry.
fn append_ledger(dir: &Path, pairs: &[(String, String)]) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _, Write as _};
    let backup = dir.join(LEGACY_BACKUP_DIR);
    fs::create_dir_all(&backup)?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(backup.join(ADOPTION_LEDGER))?;
    let mut batch = String::new();
    if f.metadata()?.len() > 0 {
        let mut last = [0u8];
        f.seek(std::io::SeekFrom::End(-1))?;
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            batch.push('\n');
        }
    }
    for (legacy, adopted) in pairs {
        batch.push_str(legacy);
        batch.push('\t');
        batch.push_str(adopted);
        batch.push('\n');
    }
    f.write_all(batch.as_bytes())
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
/// ignores I/O errors). Clearing a cover removes every extension it was
/// saved under.
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

/// Remove every file in `dir` whose stem is `stem`, except `keep` (the cover
/// just written). Best-effort, like [`remove_by_stem`].
fn remove_by_stem_except(dir: &Path, stem: &str, keep: &Path) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path != keep && path.is_file() && path.file_stem().and_then(|s| s.to_str()) == Some(stem)
        {
            let _ = fs::remove_file(&path);
        }
    }
}

impl CoverStore {
    /// Load the store: ensure `<config>/covers` exists, then scan it and record
    /// every file's stem, discarding any `.partial` copy a crash left behind.
    /// Best-effort: a missing/unreadable directory just yields an empty `known`
    /// set (nothing has a custom cover yet). Never panics.
    pub(crate) fn load() -> Self {
        let dir = covers_dir();
        let _ = fs::create_dir_all(&dir);
        let mut known = HashSet::new();
        if let Ok(read) = fs::read_dir(&dir) {
            for entry in read.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                if path.extension().is_some_and(|e| e == PARTIAL_EXT) {
                    let _ = fs::remove_file(&path);
                } else if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    known.insert(stem.to_string());
                }
            }
        }
        Self { dir, known }
    }

    /// Validate `src` (an accepted image extension, lowercased, a real file at
    /// most [`COVER_MAX_BYTES`]) and copy it into the store as `<stem>.<ext>`
    /// (crash-safely, see [`copy_into_place`]). Only once the new file is in
    /// place is any other `<stem>.*` removed, so a failed copy never costs the
    /// previous cover and a replacement under a different extension doesn't
    /// orphan the old file. Returns `true` iff
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
        let dst = self.dir.join(format!("{stem}.{ext}"));
        if copy_into_place(src, &dst).is_err() {
            return false;
        }
        remove_by_stem_except(&self.dir, &stem, &dst);
        self.known.insert(stem);
        true
    }

    /// Set artist `name`'s custom avatar from the image file `src`.
    pub(crate) fn set_artist_cover(&mut self, name: &str, src: &Path) -> bool {
        self.set_cover(artist_stem(name), src)
    }

    /// Set the album's custom cover from the image file `src`. Refused (`false`)
    /// for a title-only identity (see [`AlbumIdentity::is_located`]).
    pub(crate) fn set_album_cover(&mut self, id: &AlbumIdentity, src: &Path) -> bool {
        id.is_located() && self.set_cover(album_identity_stem(id), src)
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
    /// (never overwritten) or adopted this file before (the [`ADOPTION_LEDGER`],
    /// so a cleared cover stays cleared). An album with only a title-only
    /// identity counts as a failed copy. Only when every copy for a stem
    /// succeeded AND `may_retire` (every configured source has landed, so no
    /// album that showed it can still appear) is the legacy file moved to
    /// [`LEGACY_BACKUP_DIR`] (kept, never overwriting an earlier backup). A legacy
    /// file matching no listed album stays for a later library. Idempotent.
    pub(crate) fn adopt_legacy_album_covers<'a>(
        &mut self,
        albums: impl IntoIterator<Item = (&'a str, &'a AlbumIdentity)>,
        may_retire: bool,
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
        let ledger = read_ledger(&self.dir);
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
            let mut adopted = Vec::new();
            for id in ids {
                if !id.is_located() {
                    all_ok = false;
                    continue;
                }
                let dst_stem = album_identity_stem(id);
                if self.known.contains(&dst_stem)
                    || ledger.contains(&(stem.clone(), dst_stem.clone()))
                {
                    continue;
                }
                match copy_into_place(&src, &self.dir.join(format!("{dst_stem}.{ext}"))) {
                    Ok(()) => {
                        self.known.insert(dst_stem.clone());
                        adopted.push((stem.clone(), dst_stem));
                        out.adopted += 1;
                    }
                    Err(err) => {
                        tracing::warn!(%stem, error = %err, "could not adopt a title-keyed album cover");
                        all_ok = false;
                    }
                }
            }
            if !adopted.is_empty()
                && let Err(err) = append_ledger(&self.dir, &adopted)
            {
                tracing::warn!(%stem, error = %err, "could not record adopted album covers");
            }
            let backup = self.dir.join(LEGACY_BACKUP_DIR);
            let retired = all_ok
                && may_retire
                && fs::create_dir_all(&backup).is_ok()
                && src
                    .file_name()
                    .is_some_and(|name| fs::rename(&src, free_backup_path(&backup, name)).is_ok());
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
        let out = s.adopt_legacy_album_covers(
            [
                ("Footprints of the Traveler", &a),
                ("Footprints of the Traveler", &b),
                ("Other", &other),
            ],
            true,
        );
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
            s.adopt_legacy_album_covers([("Footprints of the Traveler", &a)], true),
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
        let out = s.adopt_legacy_album_covers([("Something Else", &id("x"))], true);
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
        let out = s.adopt_legacy_album_covers([("Box", &a), ("Box", &b)], true);
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
        let out = s.adopt_legacy_album_covers([("Box", &a), ("Box", &b)], true);
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
        let out = s.adopt_legacy_album_covers([("Box", &a)], true);
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

    /// The stems of every file directly in the covers dir.
    fn stems_on_disk() -> Vec<String> {
        fs::read_dir(covers_dir())
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| {
                e.path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
            })
            .collect()
    }

    /// A title-only (fallback) identity cannot tell same-title albums apart, so
    /// adopting into it would leak the cover to every edition: it counts as a
    /// failed copy, writes nothing for it, and keeps the legacy file.
    #[test]
    fn a_fallback_identity_never_adopts_and_keeps_the_legacy_file() {
        let _iso = config::use_isolated_test_config_dir();
        let legacy = legacy_cover("Box");
        let located = id("\u{1f}d:/m/box\u{1f}Box");
        let fallback = id(&soul_player_host::unlocated_album_identity_key("Box"));
        assert!(!fallback.is_located() && located.is_located());
        let mut s = CoverStore::load();
        let out = s.adopt_legacy_album_covers([("Box", &located), ("Box", &fallback)], true);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 1,
                retired: 0,
                kept: 1
            }
        );
        assert!(legacy.exists(), "kept for a later, fully located library");
        assert!(s.album_cover_path(&fallback).is_none());
        assert!(
            !stems_on_disk().contains(&album_identity_stem(&fallback)),
            "no file is written under the fallback identity"
        );
    }

    /// Nothing can be saved under a title-only identity either.
    #[test]
    fn a_cover_cannot_be_set_for_a_fallback_identity() {
        let _iso = config::use_isolated_test_config_dir();
        let fallback = id(&soul_player_host::unlocated_album_identity_key("Box"));
        let mut s = CoverStore::load();
        assert!(!s.set_album_cover(&fallback, &write_temp_png()));
        assert!(s.album_cover_path(&fallback).is_none());
    }

    /// While a source is still missing (offline, disabled, reconnecting), the
    /// copies happen but the legacy file stays: an album that only that source
    /// carries must still be able to adopt it once it lands.
    #[test]
    fn retirement_waits_until_the_source_set_is_complete() {
        let _iso = config::use_isolated_test_config_dir();
        let legacy = legacy_cover("Box");
        let (local, remote) = (id("local"), id("remote"));
        let mut s = CoverStore::load();
        let out = s.adopt_legacy_album_covers([("Box", &local)], false);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 1,
                retired: 0,
                kept: 1
            }
        );
        assert!(legacy.exists());
        assert!(s.album_cover_path(&local).is_some());
        let out = s.adopt_legacy_album_covers([("Box", &local), ("Box", &remote)], true);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 1,
                retired: 1,
                kept: 0
            }
        );
        assert!(s.album_cover_path(&remote).is_some());
        assert!(!legacy.exists());
    }

    /// Clearing an adopted cover sticks: while the legacy file is still pending
    /// retirement, later passes (and a restart) do not copy it back.
    #[test]
    fn a_cleared_adopted_cover_is_not_readopted_while_the_legacy_file_is_pending() {
        let _iso = config::use_isolated_test_config_dir();
        legacy_cover("Box");
        let a = id("a");
        let mut s = CoverStore::load();
        assert_eq!(s.adopt_legacy_album_covers([("Box", &a)], false).adopted, 1);
        s.clear_album_cover(&a);
        assert_eq!(s.adopt_legacy_album_covers([("Box", &a)], false).adopted, 0);
        let mut reloaded = CoverStore::load();
        assert_eq!(
            reloaded.adopt_legacy_album_covers([("Box", &a)], true),
            LegacyCoverAdoption {
                adopted: 0,
                retired: 1,
                kept: 0
            }
        );
        assert!(reloaded.album_cover_path(&a).is_none());
    }

    /// Two titles that slugify to one legacy stem both showed that cover, so
    /// both adopt it.
    #[test]
    fn two_titles_with_one_slug_both_adopt() {
        let _iso = config::use_isolated_test_config_dir();
        legacy_cover("AC/DC");
        let (a, b) = (id("a"), id("b"));
        let mut s = CoverStore::load();
        let out = s.adopt_legacy_album_covers([("AC/DC", &a), ("AC DC", &b)], true);
        assert_eq!(
            out,
            LegacyCoverAdoption {
                adopted: 2,
                retired: 1,
                kept: 0
            }
        );
        assert!(s.album_cover_path(&a).is_some() && s.album_cover_path(&b).is_some());
    }

    /// Retiring never overwrites an earlier backup of the same name.
    #[test]
    fn retiring_never_clobbers_an_earlier_backup() {
        let _iso = config::use_isolated_test_config_dir();
        let legacy = legacy_cover("Box");
        let backup = covers_dir().join(LEGACY_BACKUP_DIR);
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join("album-box.png"), b"older").unwrap();
        let mut s = CoverStore::load();
        let out = s.adopt_legacy_album_covers([("Box", &id("a"))], true);
        assert_eq!(out.retired, 1);
        assert!(!legacy.exists());
        assert_eq!(fs::read(backup.join("album-box.png")).unwrap(), b"older");
        assert_eq!(fs::read(backup.join("album-box (2).png")).unwrap(), PNG_SIG);
    }

    /// Copies go through a `.partial` temp file renamed into place, so a crash
    /// mid-copy never leaves a truncated file under a cover stem; a leftover
    /// partial is discarded on load and is never a cover.
    #[test]
    fn a_leftover_partial_copy_is_discarded_and_never_a_cover() {
        let _iso = config::use_isolated_test_config_dir();
        let a = id("a");
        let dir = covers_dir();
        fs::create_dir_all(&dir).unwrap();
        let partial = dir.join(format!("{}.png.partial", album_identity_stem(&a)));
        fs::write(&partial, &PNG_SIG[..3]).unwrap();
        let s = CoverStore::load();
        assert!(s.album_cover_path(&a).is_none());
        assert!(!partial.exists(), "the crash leftover is cleaned up");
        legacy_cover("Box");
        let mut s = CoverStore::load();
        s.adopt_legacy_album_covers([("Box", &a)], true);
        assert!(
            fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .all(|e| e.path().extension().is_none_or(|x| x != "partial")),
            "no temp file is left behind"
        );
    }

    /// A replacement whose copy fails leaves the previous cover in place: the
    /// new image is copied first, and only then are the old files removed.
    #[test]
    fn a_failed_replacement_keeps_the_previous_cover() {
        let _iso = config::use_isolated_test_config_dir();
        let a = id("a");
        let mut s = CoverStore::load();
        assert!(s.set_album_cover(&a, &write_temp_png()));
        let old = s.album_cover_path(&a).unwrap();
        // Force the copy to fail: a directory where the in-flight copy goes.
        let blocker = covers_dir().join(format!("{}.jpg.{PARTIAL_EXT}", album_identity_stem(&a)));
        fs::create_dir_all(&blocker).unwrap();
        assert!(!s.set_album_cover(&a, &write_temp_file("new.jpg", b"\xff\xd8\xff\xe0jpg")));
        assert!(
            old.exists(),
            "the previous cover survives a failed replacement"
        );
        assert_eq!(s.album_cover_path(&a), Some(old));
    }

    /// A ledger whose last line was torn (no trailing newline) does not swallow
    /// the next batch: the append starts on a fresh line.
    #[test]
    fn a_torn_ledger_line_does_not_corrupt_the_next_append() {
        let _iso = config::use_isolated_test_config_dir();
        legacy_cover("Box");
        let backup = covers_dir().join(LEGACY_BACKUP_DIR);
        fs::create_dir_all(&backup).unwrap();
        fs::write(backup.join(ADOPTION_LEDGER), "album-bo").unwrap();
        let a = id("a");
        let mut s = CoverStore::load();
        assert_eq!(s.adopt_legacy_album_covers([("Box", &a)], false).adopted, 1);
        s.clear_album_cover(&a);
        assert_eq!(
            CoverStore::load()
                .adopt_legacy_album_covers([("Box", &a)], false)
                .adopted,
            0,
            "the adoption was recorded on its own line"
        );
    }
}
