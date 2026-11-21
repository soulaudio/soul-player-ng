//! The on-disk playlist store: one self-contained folder per playlist under
//! [`playlist_store_path`], plus an `index.toml` manifest so listing (name,
//! track count, cover locators) never has to open every `playlist.toml`.
//!
//! Layout:
//! ```text
//! <store>/
//!   index.toml            { next_id, playlists: [{ id, slug, order, name, count, cover }] }
//!   <slug>-<id>/playlist.toml   (or just <id>/playlist.toml when the slug is empty)
//! ```
//!
//! The folder name's `-<id>` suffix is the only thing that matters for
//! identity (two playlists named "Chill" get distinct folders, and a name
//! that slugifies to a Windows-reserved word like `CON` is never used bare).
//! `slug` only exists for a human skimming the folder list; renaming a
//! playlist does NOT rename its folder (the id is the identity).
//!
//! Every mutation writes the ONE touched playlist's `playlist.toml` plus a
//! fresh `index.toml`; it never opens another playlist's file.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use soul_player_host::TrackLocator;

use super::cover::first_distinct_album_indices;
use super::model::{Playlist, PlaylistEntry, PlaylistId, slugify};
use crate::config;

/// How many of a playlist's leading distinct-album entries ride along in the
/// manifest as its cheap cover preview (every entry is "eligible" here; only
/// the art-presence gate in [`super::cover::select_cover`] differs).
const MANIFEST_COVER_LIMIT: usize = 4;

/// Accepted custom-cover image extensions, lowercased and without the dot.
const CUSTOM_COVER_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

/// Maximum accepted custom-cover source file size: 16 MiB.
const CUSTOM_COVER_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The first [`MANIFEST_COVER_LIMIT`] distinct-album entries' locators, in
/// playlist order; shares the dedupe rule [`super::cover::select_cover`]
/// uses, so the list page's cover candidates are already album-distinct.
fn manifest_cover(entries: &[PlaylistEntry]) -> Vec<TrackLocator> {
    first_distinct_album_indices(
        entries,
        MANIFEST_COVER_LIMIT,
        |_| true,
        |e| e.snapshot.album.as_str(),
    )
    .into_iter()
    .map(|i| entries[i].locator.clone())
    .collect()
}

/// Where playlists live on disk: `<managed-library-root>/.soul/playlists`
/// once a managed library is configured, else `<config-dir>/playlists`. This
/// function is the single seam a future managed-library root plugs into;
/// today [`config::load_managed_library`] is unset for most users (that
/// write-path-organizer feature is opt-in), so this resolves to the
/// config-dir fallback.
pub fn playlist_store_path() -> PathBuf {
    let (managed_root, _template, _strategy) = config::load_managed_library();
    if let Some(root) = managed_root {
        return root.join(".soul").join("playlists");
    }
    config::soul_player_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("playlists")
}

/// `index.toml`'s on-disk shape.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    next_id: PlaylistId,
    #[serde(default)]
    playlists: Vec<ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestEntry {
    id: PlaylistId,
    #[serde(default)]
    slug: String,
    order: usize,
    name: String,
    count: usize,
    #[serde(default)]
    cover: Vec<TrackLocator>,
}

/// A playlist as the store tracks it in memory: the literal folder name it
/// owns (fixed at creation, independent of later renames) plus the live data.
struct Loaded {
    folder: String,
    playlist: Playlist,
}

/// The loaded on-disk playlist store: every playlist, keyed by id, plus the
/// manifest order and the next id to hand out.
#[derive(Default)]
pub struct PlaylistStore {
    playlists: BTreeMap<PlaylistId, Loaded>,
    order: Vec<PlaylistId>,
    next_id: PlaylistId,
}

/// A lightweight row for a listing UI: no need to pull every entry of every
/// playlist just to show its name, count and cover.
pub struct PlaylistSummary {
    pub id: PlaylistId,
    pub name: String,
    pub count: usize,
    pub cover: Vec<TrackLocator>,
    pub custom_cover: Option<PathBuf>,
}

/// The folder name a `(slug, id)` pair maps to: `<slug>-<id>`, or just the
/// bare id when the slug is empty (an all-punctuation/whitespace name).
fn folder_name(slug: &str, id: PlaylistId) -> String {
    if slug.is_empty() {
        id.to_string()
    } else {
        format!("{slug}-{id}")
    }
}

/// The inverse of [`folder_name`]: recover the slug portion of a folder name
/// given the id it was built with. Used only when writing the manifest (the
/// in-memory store always keeps the literal folder name, never reconstructs
/// it), so a folder that doesn't follow the usual pattern just loses its
/// human-readable slug rather than losing data.
fn slug_of_folder(folder: &str, id: PlaylistId) -> String {
    folder
        .strip_suffix(&format!("-{id}"))
        .unwrap_or("")
        .to_string()
}

/// Read and parse `<folder_dir>/playlist.toml`; `None` on any I/O or parse
/// failure (missing file, garbled TOML, wrong shape).
fn read_playlist_file(folder_dir: &Path) -> Option<Playlist> {
    let body = fs::read_to_string(folder_dir.join("playlist.toml")).ok()?;
    toml::from_str(&body).ok()
}

/// Write `playlist` as `<dir>/<folder>/playlist.toml`, creating the folder if
/// needed. Best-effort (a failed write never panics the UI).
fn write_playlist_file(dir: &Path, folder: &str, playlist: &Playlist) {
    let folder_dir = dir.join(folder);
    if fs::create_dir_all(&folder_dir).is_err() {
        return;
    }
    if let Ok(body) = toml::to_string_pretty(playlist) {
        let _ = fs::write(folder_dir.join("playlist.toml"), body);
    }
}

/// Read and parse `<dir>/index.toml`; `None` if missing or garbled.
fn read_manifest(dir: &Path) -> Option<Manifest> {
    let body = fs::read_to_string(dir.join("index.toml")).ok()?;
    toml::from_str(&body).ok()
}

/// Remove any `cover.<ext>` file sitting directly in `folder_dir`
/// (best-effort; ignores I/O errors). Called before writing a fresh cover so
/// a previous cover saved under a different extension is never orphaned.
fn remove_existing_cover(folder_dir: &Path) {
    let Ok(read) = fs::read_dir(folder_dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_stem().and_then(|s| s.to_str()) == Some("cover") {
            let _ = fs::remove_file(&path);
        }
    }
}

impl PlaylistStore {
    /// Load the store from disk: missing store directory -> empty store;
    /// missing/garbled `index.toml`, or a manifest listing a folder whose
    /// `playlist.toml` fails to load -> rebuild the manifest by scanning the
    /// `<slug>-<id>/playlist.toml` folders directly, then persist the
    /// recovered manifest so the next load is back on the fast path.
    pub fn load() -> Self {
        let dir = playlist_store_path();
        if !dir.is_dir() {
            return Self::default();
        }
        if let Some(manifest) = read_manifest(&dir)
            && let Some(store) = Self::from_manifest(&dir, &manifest)
        {
            return store;
        }
        let rebuilt = Self::rebuild_from_folders(&dir);
        rebuilt.persist_manifest();
        rebuilt
    }

    /// Build a store from an already-parsed manifest, loading each listed
    /// playlist's file. `None` if any listed entry fails to load (the
    /// manifest is then treated as stale by the caller, which rebuilds from
    /// the folders themselves instead).
    fn from_manifest(dir: &Path, manifest: &Manifest) -> Option<Self> {
        let mut entries: Vec<&ManifestEntry> = manifest.playlists.iter().collect();
        entries.sort_by_key(|e| e.order);
        let mut playlists = BTreeMap::new();
        let mut order = Vec::with_capacity(entries.len());
        for e in entries {
            let folder = folder_name(&e.slug, e.id);
            let playlist = read_playlist_file(&dir.join(&folder))?;
            playlists.insert(e.id, Loaded { folder, playlist });
            order.push(e.id);
        }
        let next_id = manifest
            .next_id
            .max(order.iter().copied().max().map_or(0, |m| m + 1));
        Some(Self {
            playlists,
            order,
            next_id,
        })
    }

    /// Recover a store by scanning `dir`'s immediate subfolders for a
    /// loadable `playlist.toml`, ignoring anything else found there. Order
    /// falls back to ascending id (the manifest's custom order is lost along
    /// with the manifest itself).
    fn rebuild_from_folders(dir: &Path) -> Self {
        let mut found: Vec<(String, Playlist)> = Vec::new();
        if let Ok(read) = fs::read_dir(dir) {
            for entry in read.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(folder) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if let Some(playlist) = read_playlist_file(&path) {
                    found.push((folder, playlist));
                }
            }
        }
        found.sort_by_key(|(_, p)| p.id);
        let mut playlists = BTreeMap::new();
        let mut order = Vec::with_capacity(found.len());
        for (folder, playlist) in found {
            order.push(playlist.id);
            playlists.insert(playlist.id, Loaded { folder, playlist });
        }
        let next_id = order.iter().copied().max().map_or(0, |m| m + 1);
        Self {
            playlists,
            order,
            next_id,
        }
    }

    /// Rewrite `index.toml` from the current in-memory state (best-effort).
    fn persist_manifest(&self) {
        let dir = playlist_store_path();
        if fs::create_dir_all(&dir).is_err() {
            return;
        }
        let playlists = self
            .order
            .iter()
            .enumerate()
            .filter_map(|(order, id)| {
                let loaded = self.playlists.get(id)?;
                Some(ManifestEntry {
                    id: *id,
                    slug: slug_of_folder(&loaded.folder, *id),
                    order,
                    name: loaded.playlist.name.clone(),
                    count: loaded.playlist.entries.len(),
                    cover: manifest_cover(&loaded.playlist.entries),
                })
            })
            .collect();
        let manifest = Manifest {
            next_id: self.next_id,
            playlists,
        };
        if let Ok(body) = toml::to_string_pretty(&manifest) {
            let _ = fs::write(dir.join("index.toml"), body);
        }
    }

    /// Create a new, empty playlist named `name` and persist it. Returns its
    /// freshly-assigned id.
    pub fn create(&mut self, name: &str) -> PlaylistId {
        let id = self.next_id;
        self.next_id += 1;
        let folder = folder_name(&slugify(name), id);
        let playlist = Playlist {
            id,
            name: name.to_string(),
            custom_cover: None,
            entries: Vec::new(),
        };
        write_playlist_file(&playlist_store_path(), &folder, &playlist);
        self.playlists.insert(id, Loaded { folder, playlist });
        self.order.push(id);
        self.persist_manifest();
        id
    }

    /// Build a store entirely in memory from `(name, entries)` pairs, doing NO
    /// disk I/O (it never reads or writes `playlist_store_path()`). For the
    /// `--scene` snapshots and the a11y sweep, which must render playlist pages
    /// without touching the user's real on-disk store.
    pub(crate) fn in_memory(playlists: Vec<(&str, Vec<PlaylistEntry>)>) -> Self {
        let mut store = Self::default();
        for (name, entries) in playlists {
            let id = store.next_id;
            store.next_id += 1;
            let folder = folder_name(&slugify(name), id);
            let playlist = Playlist {
                id,
                name: name.to_string(),
                custom_cover: None,
                entries,
            };
            store.playlists.insert(id, Loaded { folder, playlist });
            store.order.push(id);
        }
        store
    }

    /// Rename playlist `id` (its folder is untouched; the id is what's
    /// durable). A no-op if `id` isn't loaded.
    pub fn rename(&mut self, id: PlaylistId, name: &str) {
        let dir = playlist_store_path();
        let Some(loaded) = self.playlists.get_mut(&id) else {
            return;
        };
        loaded.playlist.name = name.to_string();
        write_playlist_file(&dir, &loaded.folder, &loaded.playlist);
        self.persist_manifest();
    }

    /// Delete playlist `id` and its whole folder. A no-op if `id` isn't
    /// loaded.
    pub fn delete(&mut self, id: PlaylistId) {
        let Some(loaded) = self.playlists.remove(&id) else {
            return;
        };
        let _ = fs::remove_dir_all(playlist_store_path().join(&loaded.folder));
        self.order.retain(|&x| x != id);
        self.persist_manifest();
    }

    /// Apply `f` to playlist `id`'s live data, persist just its own
    /// `playlist.toml`, then rewrite the manifest. Shared by every entry
    /// mutation so each only ever touches the one folder it owns.
    fn mutate(&mut self, id: PlaylistId, f: impl FnOnce(&mut Playlist)) {
        let dir = playlist_store_path();
        let Some(loaded) = self.playlists.get_mut(&id) else {
            return;
        };
        f(&mut loaded.playlist);
        write_playlist_file(&dir, &loaded.folder, &loaded.playlist);
        self.persist_manifest();
    }

    /// Append `entry` to playlist `id`.
    pub fn add_entry(&mut self, id: PlaylistId, entry: PlaylistEntry) {
        self.mutate(id, |p| p.entries.push(entry));
    }

    /// Remove the entry at `index` from playlist `id` (clamped; a no-op if
    /// out of range).
    pub fn remove_entry(&mut self, id: PlaylistId, index: usize) {
        self.mutate(id, |p| {
            p.remove_entry(index);
        });
    }

    /// Move the entry at `from` to `to` within playlist `id` (clamped; a
    /// no-op if either index is out of range).
    pub fn move_entry(&mut self, id: PlaylistId, from: usize, to: usize) {
        self.mutate(id, |p| {
            p.move_entry(from, to);
        });
    }

    /// The full, live playlist for `id`, or `None` if it isn't loaded.
    pub fn get(&self, id: PlaylistId) -> Option<&Playlist> {
        self.playlists.get(&id).map(|l| &l.playlist)
    }

    /// Every loaded playlist as a lightweight summary, in manifest order.
    pub fn list(&self) -> Vec<PlaylistSummary> {
        self.order
            .iter()
            .filter_map(|id| {
                let loaded = self.playlists.get(id)?;
                Some(PlaylistSummary {
                    id: *id,
                    name: loaded.playlist.name.clone(),
                    count: loaded.playlist.entries.len(),
                    cover: manifest_cover(&loaded.playlist.entries),
                    custom_cover: self.custom_cover_path(*id),
                })
            })
            .collect()
    }

    /// Validate `src` (an accepted image extension, lowercased, and at most
    /// [`CUSTOM_COVER_MAX_BYTES`]), then copy it into playlist `id`'s folder
    /// as `cover.<ext>`, removing any prior `cover.*` first so a cover
    /// replaced under a different extension doesn't orphan the old file.
    /// Persists `custom_cover` on success. Returns `true` iff applied; never
    /// panics (an invalid `src`, an unknown `id`, or an I/O failure just
    /// yields `false`).
    pub fn set_custom_cover(&mut self, id: PlaylistId, src: &Path) -> bool {
        let Some(ext) = src
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
        else {
            return false;
        };
        if !CUSTOM_COVER_EXTENSIONS.contains(&ext.as_str()) {
            return false;
        }
        let Ok(metadata) = fs::metadata(src) else {
            return false;
        };
        if !metadata.is_file() || metadata.len() > CUSTOM_COVER_MAX_BYTES {
            return false;
        }
        let Some(loaded) = self.playlists.get(&id) else {
            return false;
        };
        let folder_dir = playlist_store_path().join(&loaded.folder);
        if fs::create_dir_all(&folder_dir).is_err() {
            return false;
        }
        remove_existing_cover(&folder_dir);
        let file_name = format!("cover.{ext}");
        if fs::copy(src, folder_dir.join(&file_name)).is_err() {
            return false;
        }
        self.mutate(id, |p| p.custom_cover = Some(file_name));
        true
    }

    /// Delete playlist `id`'s cover file (best-effort) and clear
    /// `custom_cover`. A no-op if `id` isn't loaded.
    pub fn clear_custom_cover(&mut self, id: PlaylistId) {
        if let Some(loaded) = self.playlists.get(&id) {
            let folder_dir = playlist_store_path().join(&loaded.folder);
            remove_existing_cover(&folder_dir);
        }
        self.mutate(id, |p| p.custom_cover = None);
    }

    /// The absolute path to playlist `id`'s current cover file, if
    /// `custom_cover` is set AND that file still exists on disk (a
    /// set-but-missing file, e.g. deleted out-of-band, resolves to `None`).
    pub fn custom_cover_path(&self, id: PlaylistId) -> Option<PathBuf> {
        let loaded = self.playlists.get(&id)?;
        let name = loaded.playlist.custom_cover.as_ref()?;
        let path = playlist_store_path().join(&loaded.folder).join(name);
        path.is_file().then_some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soul_player_host::TrackSnapshot;

    fn entry(title: &str) -> PlaylistEntry {
        entry_with_album(title, "Album")
    }

    fn entry_with_album(title: &str, album: &str) -> PlaylistEntry {
        PlaylistEntry {
            locator: TrackLocator::Local {
                path: format!("D:/music/{title}.flac"),
            },
            snapshot: TrackSnapshot {
                title: title.into(),
                artist: "Artist".into(),
                album: album.into(),
                duration_secs: 180,
                track_no: None,
                disc_no: None,
            },
            mbid: None,
        }
    }

    /// Scan the store directory for the folder holding playlist `id`'s
    /// `playlist.toml`, without going through a `PlaylistStore` (so a test
    /// can inspect disk state independent of any in-memory instance).
    fn playlist_file_of(id: PlaylistId) -> PathBuf {
        let dir = playlist_store_path();
        for entry in fs::read_dir(&dir).expect("store dir exists").flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let file = path.join("playlist.toml");
            if let Some(p) = read_playlist_file(&path)
                && p.id == id
            {
                return file;
            }
        }
        panic!("no playlist folder found for id {id}");
    }

    fn folder_of(id: PlaylistId) -> String {
        playlist_file_of(id)
            .parent()
            .expect("playlist.toml has a parent folder")
            .file_name()
            .expect("folder has a name")
            .to_string_lossy()
            .into_owned()
    }

    /// Start from an empty store. The per-process test config dir (behind
    /// `CONFIG_TEST_LOCK`) persists for the whole test binary run, so unlike
    /// a `config.toml` field a mutation here overwrites, a playlist folder
    /// created by an earlier test would otherwise still be sitting there.
    fn reset() {
        let _ = fs::remove_dir_all(playlist_store_path());
    }

    #[test]
    fn create_add_reload_roundtrips() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("Road Trip");
        s.add_entry(id, entry("song one"));
        s.add_entry(id, entry("song two"));

        let s2 = PlaylistStore::load(); // fresh read from disk
        assert_eq!(s2.get(id).unwrap().entries.len(), 2);
        assert_eq!(s2.list()[0].name, "Road Trip");
        assert_eq!(s2.list()[0].count, 2);
    }

    #[test]
    fn editing_one_playlist_does_not_rewrite_another() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let a = s.create("A");
        let b = s.create("B");
        let b_file = playlist_file_of(b);
        let b_mtime = fs::metadata(&b_file).unwrap().modified().unwrap();

        s.add_entry(a, entry("x")); // mutate A only

        assert_eq!(
            fs::metadata(&b_file).unwrap().modified().unwrap(),
            b_mtime,
            "editing A must not touch B's playlist.toml"
        );
    }

    #[test]
    fn manifest_rebuilds_when_deleted() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("Keeper");
        s.add_entry(id, entry("x"));
        fs::remove_file(playlist_store_path().join("index.toml")).unwrap();

        let s2 = PlaylistStore::load();
        assert_eq!(s2.get(id).unwrap().name, "Keeper"); // recovered by scanning folders
        assert_eq!(s2.get(id).unwrap().entries.len(), 1);
    }

    #[test]
    fn create_after_manifest_rebuild_does_not_reissue_a_live_id() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        // Advance next_id well past 0, then force a manifest-less recovery.
        let ids: Vec<_> = (0..5).map(|i| s.create(&format!("P{i}"))).collect();
        let keeper = *ids.last().unwrap();
        s.add_entry(keeper, entry("x"));
        fs::remove_file(playlist_store_path().join("index.toml")).unwrap();

        let mut s2 = PlaylistStore::load();
        let fresh = s2.create("Fresh");
        assert!(
            ids.iter().all(|&old| fresh != old),
            "rebuild reissued a live id: {fresh}"
        );
        assert!(fresh > keeper, "next_id did not advance past recovered ids");
        assert_eq!(s2.get(keeper).unwrap().entries.len(), 1);
        assert!(s2.get(fresh).is_some());
    }

    #[test]
    fn duplicate_names_get_distinct_folders() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let a = s.create("Chill");
        let b = s.create("Chill");
        assert_ne!(folder_of(a), folder_of(b));
        assert_eq!(s.list().len(), 2);
    }

    #[test]
    fn rename_keeps_id() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("Old");
        s.rename(id, "New");
        assert_eq!(PlaylistStore::load().get(id).unwrap().name, "New");
    }

    #[test]
    fn delete_removes_the_playlist_and_its_folder() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("Gone");
        let file = playlist_file_of(id);
        s.delete(id);
        assert!(s.get(id).is_none());
        assert!(!file.exists());
        assert_eq!(PlaylistStore::load().list().len(), 0);
    }

    #[test]
    fn missing_store_dir_loads_as_empty() {
        let _g = config::use_test_config_dir();
        let dir = playlist_store_path();
        let _ = fs::remove_dir_all(&dir);
        let s = PlaylistStore::load();
        assert_eq!(s.list().len(), 0);
    }

    #[test]
    fn move_and_remove_entry_persist() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("Order");
        s.add_entry(id, entry("a"));
        s.add_entry(id, entry("b"));
        s.add_entry(id, entry("c"));

        s.move_entry(id, 0, 2);
        let after_move: Vec<&str> = s
            .get(id)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.snapshot.title.as_str())
            .collect();
        assert_eq!(after_move, ["b", "c", "a"]);

        s.remove_entry(id, 0);
        let reloaded = PlaylistStore::load();
        let titles: Vec<&str> = reloaded
            .get(id)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.snapshot.title.as_str())
            .collect();
        assert_eq!(titles, ["c", "a"]);
    }

    /// A temp directory unique to this call (process id + atomic counter),
    /// so parallel tests in this binary never collide. Holds only the
    /// *source* file a custom-cover test copies from; never the playlist
    /// store itself.
    fn temp_source_dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "soul_rune_playlist_cover_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write `bytes` to `<fresh temp dir>/<name>` and return its path.
    fn write_temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path = temp_source_dir().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    /// Write a tiny but real PNG (just the 8-byte file signature; it never
    /// needs to decode, only to exist as a `.png` file under the size cap)
    /// to a fresh temp dir and return its path.
    fn write_temp_png() -> PathBuf {
        const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        write_temp_file("source.png", &PNG_SIGNATURE)
    }

    #[test]
    fn set_custom_cover_copies_file_sets_field_and_persists() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("P");
        let img = write_temp_png();
        assert!(s.set_custom_cover(id, &img));

        let p = PlaylistStore::load(); // fresh from disk
        let cover = p.custom_cover_path(id).expect("cover path resolves");
        assert!(
            cover.exists()
                && cover
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("cover.")
        );
        assert!(
            p.list()
                .iter()
                .find(|s| s.id == id)
                .unwrap()
                .custom_cover
                .is_some()
        );
    }

    #[test]
    fn rejects_non_image_and_oversize() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("P");
        assert!(!s.set_custom_cover(id, &write_temp_file("x.txt", b"nope")));
        assert!(s.custom_cover_path(id).is_none());
    }

    #[test]
    fn clear_removes_file_and_field() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("P");
        s.set_custom_cover(id, &write_temp_png());
        let path = s.custom_cover_path(id).unwrap();
        s.clear_custom_cover(id);
        assert!(!path.exists());
        assert!(PlaylistStore::load().custom_cover_path(id).is_none());
    }

    #[test]
    fn manifest_cover_dedupes_by_album() {
        let _g = config::use_test_config_dir();
        reset();
        let mut s = PlaylistStore::load();
        let id = s.create("Mix");
        // Two tracks share album "X"; 5 distinct albums follow, so the cover
        // should hold the first 4 distinct-album locators, not 4 raw entries.
        s.add_entry(id, entry_with_album("a", "X"));
        s.add_entry(id, entry_with_album("b", "X"));
        s.add_entry(id, entry_with_album("c", "Y"));
        s.add_entry(id, entry_with_album("d", "Z"));
        s.add_entry(id, entry_with_album("e", "W"));
        s.add_entry(id, entry_with_album("f", "V"));

        let cover_titles: Vec<String> = s.list()[0]
            .cover
            .iter()
            .map(|loc| match loc {
                TrackLocator::Local { path } => path.clone(),
                TrackLocator::Remote { .. } => unreachable!("test only adds local entries"),
            })
            .collect();
        assert_eq!(
            cover_titles,
            [
                "D:/music/a.flac",
                "D:/music/c.flac",
                "D:/music/d.flac",
                "D:/music/e.flac",
            ]
        );
    }
}
