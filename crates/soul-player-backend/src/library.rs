//! Local music library, backed by the shared data layer's baked `.soulidx`.
//!
//! The recursive folder scan + tag reading lives in [`soul_audio_import`] (shared
//! 1:1 with the e-ink DAP). This desktop wrapper now **dogfoods the whole shared
//! pipeline**: each watched folder is scanned + baked to a `.soulidx` and read
//! back through the no-alloc [`IndexReader`] (over an *owning* in-memory source —
//! no leak, no on-disk cache), and the folders are aggregated into one browse by
//! [`MultiCatalog`]. The surface the actor + UI consume
//! ([`scan`](Library::scan) / [`path_of`](Library::path_of) /
//! [`duration_of`](Library::duration_of) / [`views`](Library::views)) is
//! unchanged — only the backing store moved from an in-memory `CatalogInput` to
//! the baked index the DAP also reads.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use embedded_io_adapters::std::FromStd;
use soul_audio_catalog::{BrowseAxis, Catalog, StrRef};
use soul_audio_desktop::{OwnedDecoder, open_track};
use soul_audio_import::{ScanCache, ScanProgress, SkippedGroup};
use soul_audio_index::IndexReader;
use soul_audio_multicatalog::{MultiCatalog, pack, unpack};
use soul_audio_playlist::{PlaylistEntry, ResolvedPlaylist};

use crate::types::{TrackLocator, TrackLoudness, TrackView};

/// The result of an [incremental scan](Library::scan_many_incremental): the baked
/// library plus the warm cache and deletions, carried back so the host can persist
/// the cache and warn about removals.
pub struct IncrementalBuild {
    /// The baked, aggregated library (identical to a full [`scan_many`](Library::scan_many)).
    pub library: Library,
    /// The updated [`ScanCache`] per root, **parallel to the `roots`** passed in —
    /// persist it (keyed by root path) to make the next startup a warm diff.
    pub caches: Vec<ScanCache>,
    /// Deletions detected since the prior cache, as `(root, removed file paths)`,
    /// only for roots that had any. These are genuine per-file removals (the host
    /// only scans *available* roots, so an offline drive never lands here).
    pub removed: Vec<(PathBuf, Vec<PathBuf>)>,
    /// Audio files the scan walked past without importing, as `(root, groups)`
    /// for each root that skipped any — counts per extension, with the reason.
    ///
    /// Carried out of the scan rather than dropped because a silent skip is
    /// indistinguishable from a broken scan: a user whose `.aac` rips never
    /// appear has no way to learn that this build's decoder is gated off. Shaped
    /// like [`removed`](Self::removed) — per affected root, empty otherwise — so
    /// the host can surface it the same way it surfaces deletions.
    pub skipped: Vec<(PathBuf, Vec<SkippedGroup>)>,
    /// The baked `.soulidx` bytes per root, **parallel to the `roots`** passed in.
    /// Each root bakes independently, so these can be cached and later fed back to
    /// [`Library::from_source_bytes`] to rebuild an unaffected root without
    /// re-scanning it — the basis for the watcher's scoped rescan.
    pub bytes: Vec<Vec<u8>>,
}

/// The result of [`Library::locate`]: which source a track belongs to and its
/// path relative to that source's root — e.g. for a remote source, whose
/// "root" isn't a real local directory, the relative path (not an absolute
/// local one) is what a caller needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// Index into [`Library::source_counts`]'s order — the compound id's
    /// source bits.
    pub source: usize,
    /// The file's path relative to that source's root, native components.
    pub rel: PathBuf,
}

/// A `.soulidx` reader over an **owning** in-memory source: `Cursor` owns the
/// baked bytes, so the reader is `'static` with no leak and no on-disk cache, and
/// drops cleanly when the `Library` is swapped out on rescan.
type MemReader = IndexReader<FromStd<Cursor<Vec<u8>>>>;

/// The desktop library: the baked catalog (one source per watched folder,
/// aggregated by [`MultiCatalog`]) plus each source's physical root, needed to
/// turn a track's `/`-separated folder path + file name back into an absolute
/// path for the decoder. Track ids are the aggregate's **compound** ids (source
/// in the high bits); for a single folder they are the plain record ids.
pub struct Library {
    /// Physical root per source, indexed by a compound id's source bits.
    roots: Vec<PathBuf>,
    /// Each source's own track count (parallel to `roots`), captured at scan
    /// time before the per-root reader is boxed into the aggregate.
    counts: Vec<usize>,
    /// The aggregate behind a `Mutex`: `MultiCatalog` is `Send` but not `Sync`
    /// (its readers use single-threaded `RefCell` interior mutability), so the
    /// shared `Arc<Library>` serializes catalog access here. The lock is held
    /// only for the duration of one public call.
    catalog: Mutex<MultiCatalog>,
}

impl Library {
    /// Scan + bake a single watched folder, read back through the shared index —
    /// the `N = 1` case of [`scan_many`](Library::scan_many).
    pub fn scan(root: &Path) -> anyhow::Result<Library> {
        Self::scan_many(&[root.to_owned()])
    }

    /// Scan + bake each watched folder (folder-isolation, tag reading, every
    /// supported format — all the importer's, shared with the DAP), reading each
    /// back through an owning [`IndexReader`] and aggregating them into one
    /// browse. Robustness (unreadable tags degrade, unreadable subdirs skip,
    /// symlink cycles terminate, only an unreadable root errors) is the
    /// importer's. Errors if any root fails to scan.
    pub fn scan_many(roots: &[PathBuf]) -> anyhow::Result<Library> {
        let mut members: Vec<Box<dyn Catalog + Send>> = Vec::with_capacity(roots.len());
        let mut counts = Vec::with_capacity(roots.len());
        for root in roots {
            let bytes = soul_audio_import::bake(root)?;
            let reader: MemReader = IndexReader::new(FromStd::new(Cursor::new(bytes)));
            counts.push(reader.track_count() as usize);
            members.push(Box::new(reader));
        }
        Ok(Library {
            roots: roots.to_vec(),
            counts,
            catalog: Mutex::new(MultiCatalog::new(members)),
        })
    }

    /// Scan + bake each watched folder **incrementally**: unchanged files are
    /// reused from `priors[i]` (the cache returned by a previous build for
    /// `roots[i]`), only new/changed files are re-read, and extraction fans out the
    /// I/O across up to `max_threads` workers while honouring a **live** `target`
    /// (shared across all roots) — the host moves `target` as CPU load changes, so a
    /// scan already in flight throttles when the machine is busy and opens back up
    /// when it frees. Produces a library byte-identical to [`scan_many`] for the same
    /// tree — it only changes *how much work* is done — plus the updated
    /// per-root [`ScanCache`] (persist it for a warm next startup) and the
    /// deletions detected per root (for a removal warning). A missing or short
    /// `priors` entry (`priors.get(i)` is `None`) is treated as an empty cache (a
    /// cold scan of that root). Errors if any root fails to scan.
    ///
    /// `progress` is forwarded per root to the underlying scan — each root reports a
    /// monotonic `done/total` within its own [`ScanPhase`], so with multiple roots
    /// the count restarts at each one (fine for the single-root common case; a
    /// coarse "root i of n" is a future refinement).
    pub fn scan_many_incremental(
        roots: &[PathBuf],
        priors: &[ScanCache],
        max_threads: usize,
        target: &AtomicUsize,
        progress: &mut dyn FnMut(ScanProgress),
        loudness: &HashMap<PathBuf, TrackLoudness>,
    ) -> anyhow::Result<IncrementalBuild> {
        let max_threads = max_threads.max(1);
        let mut members: Vec<Box<dyn Catalog + Send>> = Vec::with_capacity(roots.len());
        let mut counts = Vec::with_capacity(roots.len());
        let mut caches = Vec::with_capacity(roots.len());
        let mut all_bytes = Vec::with_capacity(roots.len());
        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        for (i, root) in roots.iter().enumerate() {
            let prior = priors.get(i).cloned().unwrap_or_default();
            let mut outcome = soul_audio_import::scan_incremental_throttled(
                root,
                &prior,
                max_threads,
                target,
                progress,
            )?;
            // Stamp any known per-track loudness into the record so the baked `.soulidx`
            // carries it (for the DAP and a rebake-persistent column); the metadata scan
            // itself never computes loudness (the deferred Tier-2 sweep does).
            if !loudness.is_empty() {
                for t in &mut outcome.input.tracks {
                    let path = root
                        .join(&outcome.input.folders[t.folder as usize].path)
                        .join(&t.file_name);
                    if let Some(m) = loudness.get(&path) {
                        t.loudness = Some((*m).into());
                    }
                }
            }
            let bytes = soul_audio_index::build::write_index(&outcome.input);
            // Keep the baked bytes so an unaffected root can be reused verbatim on a
            // scoped rescan; the reader reads a clone, the original is carried back.
            let reader: MemReader = IndexReader::new(FromStd::new(Cursor::new(bytes.clone())));
            counts.push(reader.track_count() as usize);
            members.push(Box::new(reader));
            all_bytes.push(bytes);
            if !outcome.removed.is_empty() {
                removed.push((root.clone(), outcome.removed));
            }
            // Only roots that actually skipped something: an empty entry per root
            // would make "did this scan skip anything?" a search rather than a
            // `is_empty()`, and the UI asks exactly that question.
            if !outcome.skipped.is_empty() {
                skipped.push((root.clone(), std::mem::take(&mut outcome.skipped)));
            }
            caches.push(outcome.cache);
        }
        Ok(IncrementalBuild {
            library: Library {
                roots: roots.to_vec(),
                counts,
                catalog: Mutex::new(MultiCatalog::new(members)),
            },
            caches,
            removed,
            skipped,
            bytes: all_bytes,
        })
    }

    /// Rebuild a library from per-root baked `.soulidx` bytes (parallel to `roots`),
    /// **without scanning** — the companion to the `bytes` returned by
    /// [`scan_many_incremental`](Self::scan_many_incremental). Because each root bakes
    /// independently, feeding an unaffected root's cached bytes here produces exactly
    /// what re-scanning it would, so the watcher can re-scan only the roots that
    /// changed and reuse the rest. `roots.len()` must equal `bytes.len()`.
    #[must_use]
    pub fn from_source_bytes(roots: &[PathBuf], bytes: Vec<Vec<u8>>) -> Library {
        debug_assert_eq!(roots.len(), bytes.len(), "one baked index per root");
        let mut members: Vec<Box<dyn Catalog + Send>> = Vec::with_capacity(bytes.len());
        let mut counts = Vec::with_capacity(bytes.len());
        for b in bytes {
            let reader: MemReader = IndexReader::new(FromStd::new(Cursor::new(b)));
            counts.push(reader.track_count() as usize);
            members.push(Box::new(reader));
        }
        Library {
            roots: roots.to_vec(),
            counts,
            catalog: Mutex::new(MultiCatalog::new(members)),
        }
    }

    /// Lock the aggregate for one call.
    fn cat(&self) -> std::sync::MutexGuard<'_, MultiCatalog> {
        self.catalog.lock().expect("library catalog mutex poisoned")
    }

    /// Number of tracks across all sources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cat().track_count() as usize
    }

    /// Whether the scan found no tracks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cat().track_count() == 0
    }

    /// (root, track_count) per scanned source, in scan (source-bit) order.
    #[must_use]
    pub fn source_counts(&self) -> Vec<(PathBuf, usize)> {
        self.roots
            .iter()
            .cloned()
            .zip(self.counts.iter().copied())
            .collect()
    }

    /// The absolute file path for track `id`: `roots[source] / folder.path /
    /// file_name`. The compound id's source selects the physical root; the
    /// `/`-separated, root-relative `folder.path` is re-split into native
    /// components.
    #[must_use]
    pub fn path_of(&self, id: u32) -> Option<PathBuf> {
        let located = self.locate(id)?;
        let root = self.roots.get(located.source)?;
        Some(root.join(located.rel))
    }

    /// The source index + root-relative path for track `id`, without joining
    /// it to that source's physical root — the piece [`path_of`](Self::path_of)
    /// needs before it resolves to an absolute local path, and what a remote
    /// source (whose "root" isn't a real local directory) needs instead. The
    /// compound id's source bits select the source; the `/`-separated
    /// `folder.path` is re-split into native components and joined with the
    /// file name.
    #[must_use]
    pub fn locate(&self, id: u32) -> Option<Located> {
        let (source, _) = unpack(id);
        let source = source as usize;
        // Bounds-check the source even though nothing here indexes `roots` —
        // `path_of` relies on this to reject an out-of-range id the same way
        // it always has.
        if source >= self.roots.len() {
            return None;
        }
        let cat = self.cat();
        let t = cat.track(id).ok()?;
        let folder = cat.folder(t.folder).ok()?;
        let rel_str = resolve(&cat, folder.path)?;
        let name = resolve(&cat, t.file_name)?;
        let mut rel = PathBuf::new();
        if !rel_str.is_empty() {
            for seg in rel_str.split('/') {
                rel.push(seg);
            }
        }
        rel.push(&name);
        Some(Located { source, rel })
    }

    /// A durable [`TrackLocator`] for the current track `id`: the absolute
    /// path (forward-slash normalized), independent of the volatile compound
    /// id a rescan can renumber. Playlists store this instead of the id. Every
    /// source behind this `Library` is local, so this always yields `Local`
    /// (or `None` for an id that doesn't resolve); a remote source's own
    /// locator is built by its own layer (`soul_player_host::remote`).
    #[must_use]
    pub fn locator_of(&self, id: u32) -> Option<TrackLocator> {
        let path = self.path_of(id)?;
        Some(TrackLocator::Local {
            path: path.to_string_lossy().replace('\\', "/"),
        })
    }

    /// Resolve durable locators back to **current** track ids, surviving a
    /// rescan that renumbered them. Phase 1: exact-locator (Local path) match
    /// only; `None` for anything that doesn't match (a missing file, a moved
    /// file, or a `Remote` locator, which this local-only `Library` can never
    /// produce or match). Builds a `locator-key -> id` reverse map from the
    /// current catalog once, then looks up each input by
    /// [`TrackLocator::key`] so separator/case differences don't matter.
    #[must_use]
    pub fn resolve_locators(&self, locs: &[TrackLocator]) -> Vec<Option<u32>> {
        let mut by_key: HashMap<String, u32> = HashMap::new();
        for (source, &count) in self.counts.iter().enumerate() {
            for local in 0..count as u32 {
                let id = pack(source as u32, local);
                if let Some(loc) = self.locator_of(id) {
                    by_key.insert(loc.key(), id);
                }
            }
        }
        locs.iter()
            .map(|loc| by_key.get(&loc.key()).copied())
            .collect()
    }

    /// Resolve a playlist's `entries` into the shared [`ResolvedPlaylist`] view-model
    /// (per-entry availability, the available play context + its index map, total
    /// duration) by STREAMING this library's catalog through the no_std resolver.
    /// Memory is O(playlist), not O(catalog): only the playlist's own locator keys
    /// are held, and the per-track `(key, id, duration)` tuples flow straight through
    /// without being collected, exactly as the DAP streams its SD-card `.soulidx`.
    #[must_use]
    pub fn resolve_playlist(&self, entries: &[PlaylistEntry]) -> ResolvedPlaylist {
        ResolvedPlaylist::resolve(
            entries,
            (0..self.counts.len()).flat_map(move |source| {
                (0..self.counts[source] as u32).filter_map(move |local| {
                    let id = pack(source as u32, local);
                    let loc = self.locator_of(id)?;
                    Some((loc.key(), id, self.duration_of(id)))
                })
            }),
        )
    }

    /// The tag-derived duration (seconds) for `id`, if known (> 0). Used to fill
    /// a Tick when the decoder can't report a length, keeping the progress bar
    /// consistent with the track list.
    #[must_use]
    pub fn duration_of(&self, id: u32) -> Option<f64> {
        let t = self.cat().track(id).ok()?;
        (t.duration_ms > 0).then(|| f64::from(t.duration_ms) / 1000.0)
    }

    /// The UI-facing view of every track, in **Album-Artist → Album → disc/track**
    /// order (the baked `ArtistAlbumTrack` axis — the audiophile/player default;
    /// leading articles are folded by the shared collation). The importer applies
    /// the file-name fallback for a missing title, so `title` is present.
    #[must_use]
    pub fn views(&self) -> Vec<TrackView> {
        self.views_by(BrowseAxis::ArtistAlbumTrack)
    }

    /// Track views in a chosen browse order — the baked axis tables
    /// (`Title`, `ArtistAlbumTrack`, `Album`, `Folder`), computed once by the
    /// baker so no re-sort happens here.
    #[must_use]
    pub fn views_by(&self, axis: BrowseAxis) -> Vec<TrackView> {
        use std::collections::HashMap;
        let cat = self.cat();
        let n = cat.order_len(axis);
        // Intern the low-cardinality fields by their interned id across the WHOLE
        // library, as shared `Arc<str>`: a million tracks share a few thousand
        // artists/albums (and a handful of genres/formats), so each distinct value
        // is decoded AND allocated ONCE here, then cloned per track as a cheap Arc
        // refcount bump — not one fresh `String` per track. On a big library's load
        // that's millions of fewer catalog decodes AND heap allocations, and the
        // resident `Vec<TrackView>` shares those strings instead of duplicating them.
        let mut artists: HashMap<u32, Option<Arc<str>>> = HashMap::new();
        let mut albums: HashMap<u32, Option<Arc<str>>> = HashMap::new();
        let mut genres: HashMap<u32, Option<Arc<str>>> = HashMap::new();
        let mut dates: HashMap<u32, (u32, u32)> = HashMap::new();
        // The handful of distinct FORMAT labels ("FLAC", "MP3", …), interned so a
        // million tracks share ~10 `Arc<str>` instead of allocating a label each.
        let mut formats: HashMap<Box<str>, Arc<str>> = HashMap::new();
        // Reused across tracks: each file name is decoded into THIS buffer to read
        // its extension, not a fresh per-track heap `String` that's thrown away.
        let mut name_buf: Vec<u8> = Vec::new();
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let Ok(id) = cat.order(axis, i) else { continue };
            let Ok(t) = cat.track(id) else { continue };
            let (original_date, release_date) = *dates
                .entry(t.album)
                .or_insert_with(|| album_dates(&cat, t.album));
            let artist = artists
                .entry(t.artist)
                .or_insert_with(|| real_name(artist_name(&cat, t.artist)).map(Arc::from))
                .clone();
            let album = albums
                .entry(t.album)
                .or_insert_with(|| real_name(album_title(&cat, t.album)).map(Arc::from))
                .clone();
            // Genre keeps its raw value — "Unknown Genre" is a real bucket, NOT
            // filtered through `real_name` like artist/album sentinels.
            let genre = genres
                .entry(t.genre)
                .or_insert_with(|| genre_name(&cat, t.genre).map(Arc::from))
                .clone();
            let format = decode_into(&cat, t.file_name, &mut name_buf)
                .and_then(format_label)
                .map(|label| intern_format(&mut formats, label));
            out.push(TrackView {
                id,
                title: resolve(&cat, t.title),
                artist,
                album,
                genre,
                duration_secs: f64::from(t.duration_ms) / 1000.0,
                // The backend/local catalog is always local — the host aggregate sets
                // `Some(host)` for a remote source's tracks.
                remote_source: None,
                original_date,
                release_date,
                // The baked loudness (present once a scan has stamped it); the host
                // overrides with fresher live analysis via `mark_loudness`.
                loudness: t.loudness.map(Into::into),
                format,
            });
        }
        out
    }
}

/// Resolves a track id to a local file path for the loader. `Library` resolves
/// it locally; a host-side adapter can resolve remote ids by fetching them.
pub trait TrackSource: Send + Sync {
    /// Resolve a track id to the local file path the loader should open, or `None`
    /// when the id doesn't resolve (unknown, or an unavailable source).
    fn path_of(&self, id: u32) -> Option<std::path::PathBuf>;

    /// The tag-derived duration (seconds), used by the pump's Tick fallback when
    /// the decoder itself can't report a length. Defaults to "unknown" so an
    /// implementor only needs this when it has a cheaper-than-decoding answer.
    fn duration_of(&self, _id: u32) -> Option<f64> {
        None
    }

    /// Open the decoder for track `id` — what the loader thread calls to build
    /// the `OwnedDecoder` it hands to the pump. The default resolves
    /// [`path_of`](Self::path_of) and opens the local file with
    /// [`open_track`] — all `Library` needs. A source that can decode bytes it
    /// doesn't have sitting on local disk (a remote server, streamed over HTTP
    /// Range) overrides this instead of materializing a path first — see
    /// `soul_player_host::remote::RemoteLibrary::open_decoder`.
    fn open_decoder(&self, id: u32) -> anyhow::Result<OwnedDecoder> {
        let path = self
            .path_of(id)
            .ok_or_else(|| anyhow::anyhow!("no path for track id {id}"))?;
        open_track(&path)
    }
}

impl TrackSource for Library {
    fn path_of(&self, id: u32) -> Option<std::path::PathBuf> {
        Library::path_of(self, id)
    }

    fn duration_of(&self, id: u32) -> Option<f64> {
        Library::duration_of(self, id)
    }
}

/// Resolve a heap string ref (exact length known) into an owned `String`, routed
/// through the aggregate so the owning source is honored.
fn resolve(cat: &MultiCatalog, s: StrRef) -> Option<String> {
    let mut buf = vec![0u8; s.len as usize];
    let n = cat.resolve_str(s, &mut buf).ok()?;
    buf.truncate(n);
    String::from_utf8(buf).ok()
}

/// Like [`resolve`], but decodes into a caller-owned REUSED buffer and hands back a
/// `&str` view — so a caller that only reads the string (the FORMAT label) does not
/// heap-allocate an owned `String` per call. `buf` is grown as needed and reused
/// across calls.
fn decode_into<'b>(cat: &MultiCatalog, s: StrRef, buf: &'b mut Vec<u8>) -> Option<&'b str> {
    buf.clear();
    buf.resize(s.len as usize, 0);
    let n = cat.resolve_str(s, buf).ok()?;
    std::str::from_utf8(&buf[..n]).ok()
}

/// Resolve an artist name (length not known up front) into an owned `String`.
fn artist_name(cat: &MultiCatalog, id: u32) -> Option<String> {
    let mut buf = [0u8; 512];
    let n = cat.artist_name(id, &mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// Resolve a genre's display name (length not known up front) into an owned
/// `String`, mirroring [`artist_name`]. The raw value is kept by the caller —
/// the importer's `"Unknown Genre"` is a real browse bucket, so (unlike
/// artist/album) it is NOT routed through [`real_name`].
fn genre_name(cat: &MultiCatalog, id: u32) -> Option<String> {
    let mut buf = [0u8; 512];
    let n = cat.genre_name(id, &mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// Resolve an album's title via its record.
fn album_title(cat: &MultiCatalog, id: u32) -> Option<String> {
    let album = cat.album(id).ok()?;
    resolve(cat, album.title)
}

/// The album's two packed dates (`soul_audio_catalog::PackedDate` as raw `u32`):
/// `(original_date, release_date)`, `0` when the album id is out of range.
fn album_dates(cat: &MultiCatalog, album: u32) -> (u32, u32) {
    cat.album(album)
        .map(|a| (a.original_date.0, a.release_date.0))
        .unwrap_or((0, 0))
}

/// An uppercased codec/container label for the Tracks table's FORMAT badge,
/// derived from a resolved file name's extension (lower-cased before matching,
/// so `.FLAC` and `.flac` agree). Aliases a handful of extensions to the label
/// the old app shows (`m4a` -> `M4A`, `aif`/`aiff` -> `AIFF`, `ogg`/`oga` ->
/// `OGG`); anything else known-audio just uppercases its own extension (`mp2` ->
/// `MP2`). `None` when the name has no extension: the importer only ever hands
/// this already-supported audio file names, so every real call resolves to
/// `Some`.
fn format_label(file_name: &str) -> Option<Cow<'static, str>> {
    let ext = Path::new(file_name).extension()?.to_str()?;
    // Known extensions map to a `&'static str` with NO per-track allocation (the
    // case-insensitive compare avoids even the `to_lowercase` temporary). Only an
    // unrecognized extension allocates, and that's rare — the importer only hands
    // this already-supported audio names.
    let known = |label: &'static str| Some(Cow::Borrowed(label));
    if ext.eq_ignore_ascii_case("flac") {
        known("FLAC")
    } else if ext.eq_ignore_ascii_case("mp3") {
        known("MP3")
    } else if ext.eq_ignore_ascii_case("wav") {
        known("WAV")
    } else if ext.eq_ignore_ascii_case("m4a") {
        known("M4A")
    } else if ext.eq_ignore_ascii_case("aac") {
        known("AAC")
    } else if ext.eq_ignore_ascii_case("ogg") || ext.eq_ignore_ascii_case("oga") {
        known("OGG")
    } else if ext.eq_ignore_ascii_case("opus") {
        known("OPUS")
    } else if ext.eq_ignore_ascii_case("aif") || ext.eq_ignore_ascii_case("aiff") {
        known("AIFF")
    } else if ext.eq_ignore_ascii_case("dsf") {
        known("DSF")
    } else if ext.eq_ignore_ascii_case("dff") {
        known("DFF")
    } else if ext.eq_ignore_ascii_case("wv") {
        known("WV")
    } else {
        Some(Cow::Owned(ext.to_uppercase()))
    }
}

/// Intern a FORMAT label into a shared `Arc<str>`, keyed by the label text. The
/// first track of each format allocates the key + `Arc`; every later track with
/// that format gets a cheap `Arc` clone (refcount bump), so a million-track
/// library holds ~10 format `Arc`s, not a million label `String`s.
fn intern_format(
    cache: &mut std::collections::HashMap<Box<str>, Arc<str>>,
    label: Cow<'static, str>,
) -> Arc<str> {
    if let Some(existing) = cache.get(label.as_ref()) {
        return existing.clone();
    }
    let arc: Arc<str> = Arc::from(label.as_ref());
    cache.insert(Box::from(label.as_ref()), arc.clone());
    arc
}

/// Map the importer's `"Unknown Artist"` / `"Unknown Album"` placeholders back to
/// `None`, preserving the UI's "blank, not the word Unknown" presentation for an
/// untagged file.
fn real_name(s: Option<String>) -> Option<String> {
    match s.as_deref() {
        None | Some("Unknown Artist" | "Unknown Album") => None,
        Some(_) => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quantifies the memory the `Arc<str>` interning of `TrackView`'s
    /// artist/album/genre saves over owned `String`s at scale — the delivered win,
    /// not a what-if. `#[ignore]`d; run with
    /// `cargo test -p soul-player-backend bench_string_memory -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_string_memory_at_scale() {
        use std::collections::HashSet;
        for &n in &[100_000u32, 1_000_000] {
            // ~12 tracks/album, ~8 albums/artist, 10 genres — a believable fan-out.
            let views: Vec<TrackView> = (0..n)
                .map(|i| {
                    let album = i / 12;
                    let artist = album / 8;
                    TrackView {
                        id: i,
                        title: Some(format!("Track {i}")),
                        artist: Some(Arc::from(format!("Artist {artist}"))),
                        album: Some(Arc::from(format!("Album {album}"))),
                        genre: Some(Arc::from(format!("Genre {}", artist % 10))),
                        duration_secs: 180.0,
                        remote_source: None,
                        original_date: 0,
                        release_date: 0,
                        loudness: None,
                        format: None,
                    }
                })
                .collect();
            // Owned String: a 24-byte header per field per track + its heap bytes,
            // DUPLICATED across every track that shares the value.
            let owned = |o: &Option<Arc<str>>| o.as_ref().map_or(0, |s| 24 + s.len());
            let owned_total: usize = views
                .iter()
                .map(|t| owned(&t.artist) + owned(&t.album) + owned(&t.genre))
                .sum();
            // Arc<str>: a 16-byte fat pointer per field per track, plus each DISTINCT
            // string stored once (16-byte Arc header + content).
            let mut artists = HashSet::new();
            let mut albums = HashSet::new();
            let mut genres = HashSet::new();
            for t in &views {
                t.artist.as_deref().map(|a| artists.insert(a));
                t.album.as_deref().map(|a| albums.insert(a));
                t.genre.as_deref().map(|g| genres.insert(g));
            }
            let distinct = |set: &HashSet<&str>| set.iter().map(|s| 16 + s.len()).sum::<usize>();
            let interned_total =
                views.len() * 16 * 3 + distinct(&artists) + distinct(&albums) + distinct(&genres);
            eprintln!(
                "n={n:>9}  artist/album/genre: {:>6.1} MB owned -> {:>6.1} MB Arc<str> (save {:>6.1} MB; {} artists, {} albums, {} genres)",
                owned_total as f64 / 1e6,
                interned_total as f64 / 1e6,
                (owned_total - interned_total) as f64 / 1e6,
                artists.len(),
                albums.len(),
                genres.len(),
            );
        }
    }
    use soul_audio_import::{ScanPhase, SkipReason};

    /// A minimal valid PCM WAV of `frames` stereo frames at `rate` (silence).
    fn wav(rate: u32, frames: u32) -> Vec<u8> {
        let channels: u16 = 2;
        let bits: u16 = 16;
        let block_align = channels * bits / 8;
        let byte_rate = rate * u32::from(block_align);
        let data_len = frames * u32::from(block_align);
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        v
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sas_lib_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Path to a committed `testdata/` fixture (relative to the workspace).
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata")
            .join(name)
    }

    #[test]
    fn scan_finds_supported_files_skips_others_and_assigns_index_ids() {
        let root = tmpdir("scan");
        std::fs::write(root.join("b.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("a.wav"), wav(48_000, 24_000)).unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/c.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("notes.txt"), b"not audio").unwrap();

        let lib = Library::scan(&root).unwrap();
        // Sorted walk: a.wav, b.wav, sub/c.wav — .txt skipped.
        assert_eq!(lib.len(), 3);
        let v = lib.views();
        assert_eq!(v[0].id, 0);
        assert!(lib.path_of(0).unwrap().ends_with("a.wav"));
        assert_eq!(v[2].id, 2);
        assert!(lib.path_of(2).unwrap().ends_with("c.wav"));
        assert_eq!(lib.path_of(9), None);
    }

    #[test]
    fn locate_returns_source_index_and_root_relative_path() {
        let root = tmpdir("locate");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();

        let lib = Library::scan_many(std::slice::from_ref(&root)).unwrap();
        let located = lib.locate(0).expect("track 0 locates");
        assert_eq!(located.source, 0);
        assert_eq!(
            root.join(&located.rel),
            lib.path_of(0).unwrap(),
            "root joined with the relative path matches path_of"
        );

        // Same out-of-range id that path_of (see
        // scan_finds_supported_files_skips_others_and_assigns_index_ids) returns
        // None for.
        assert_eq!(lib.locate(9), None);
    }

    #[test]
    fn locator_of_returns_the_absolute_path_for_a_local_track() {
        let root = tmpdir("locator_of");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("b.wav"), wav(48_000, 48_000)).unwrap();
        let lib = Library::scan(&root).unwrap();
        let id = lib.views()[0].id;
        let loc = lib.locator_of(id).unwrap();
        match loc {
            TrackLocator::Local { path } => assert!(path.ends_with("a.wav")),
            TrackLocator::Remote { .. } => panic!("expected a local locator"),
        }
    }

    #[test]
    fn resolve_locators_survives_a_rescan_that_renumbers_ids() {
        let root = tmpdir("rescan_renumber");
        std::fs::write(root.join("b.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("c.wav"), wav(48_000, 48_000)).unwrap();
        let lib = Library::scan(&root).unwrap();
        // Capture a durable locator for the track currently named b.wav.
        let b_id = lib
            .views()
            .iter()
            .find(|v| {
                matches!(
                    lib.locator_of(v.id),
                    Some(TrackLocator::Local { path }) if path.ends_with("b.wav")
                )
            })
            .unwrap()
            .id;
        let loc = lib.locator_of(b_id).unwrap();

        // Insert an alphabetically-earlier file: b.wav's local id shifts.
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();
        let lib2 = Library::scan(&root).unwrap();
        let resolved =
            lib2.resolve_locators(std::slice::from_ref(&loc))[0].expect("resolves after rescan");

        // The resolved id must still map back to b.wav.
        assert!(matches!(
            lib2.locator_of(resolved),
            Some(TrackLocator::Local { path }) if path.ends_with("b.wav")
        ));
        // And the raw id value should differ from before (renumbered).
        assert_ne!(resolved, b_id);
    }

    #[test]
    fn resolve_locators_returns_none_for_a_missing_path() {
        let root = tmpdir("resolve_missing");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();
        let lib = Library::scan(&root).unwrap();
        let miss = TrackLocator::Local {
            path: "D:/nope/gone.flac".into(),
        };
        assert_eq!(lib.resolve_locators(&[miss]), vec![None]);
    }

    #[test]
    fn resolve_playlist_resolves_available_context_and_totals_live_or_snapshot() {
        fn snap(secs: u32) -> crate::types::TrackSnapshot {
            crate::types::TrackSnapshot {
                title: "t".into(),
                artist: "a".into(),
                album: "al".into(),
                duration_secs: secs,
                track_no: None,
                disc_no: None,
            }
        }
        let root = tmpdir("resolve_playlist");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap(); // ~1s live
        let lib = Library::scan(&root).unwrap();
        let a_id = lib.views()[0].id;
        let a_loc = lib.locator_of(a_id).unwrap();
        let a_dur = lib.duration_of(a_id).unwrap();

        // One resolving entry (its snapshot duration is a WRONG 7s, to prove the live
        // duration wins) and one whose file is gone (greyed, snapshot 5s used).
        let entries = vec![
            PlaylistEntry {
                locator: a_loc,
                snapshot: snap(7),
                mbid: None,
            },
            PlaylistEntry {
                locator: TrackLocator::Local {
                    path: "D:/gone/x.flac".into(),
                },
                snapshot: snap(5),
                mbid: None,
            },
        ];
        let r = lib.resolve_playlist(&entries);
        assert_eq!(r.resolved, vec![Some(a_id), None]);
        assert_eq!(r.play_ids, vec![a_id]); // unavailable entry skipped
        assert_eq!(r.play_index_of, vec![Some(0), None]);
        assert_eq!(r.available_count(), 1);
        // Live duration for the resolved track (NOT its stale snapshot 7), snapshot
        // for the missing one: a_dur + 5.
        assert!((r.total_secs - (a_dur + 5.0)).abs() < 1e-6);
    }

    #[test]
    fn scan_skips_macos_appledouble_sidecars() {
        let root = tmpdir("appledouble");
        std::fs::write(root.join("01 - song.flac"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("._01 - song.flac"), b"appledouble junk").unwrap();
        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.len(), 1, "AppleDouble sidecar must be skipped");
        assert!(lib.path_of(0).unwrap().ends_with("01 - song.flac"));
    }

    #[test]
    fn scan_reads_duration() {
        let root = tmpdir("dur");
        std::fs::write(root.join("x.wav"), wav(48_000, 48_000)).unwrap(); // 1.0 s
        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.len(), 1);
        let d = lib.duration_of(0).unwrap();
        assert!((d - 1.0).abs() < 0.05, "duration {d} not ~1.0 s");
    }

    #[test]
    fn scan_many_aggregates_sources_and_routes_each_path_to_its_root() {
        // Two independently-scanned folders aggregate into one library; every
        // track locates back to ITS OWN source root (the per-source path_of).
        let a = tmpdir("multi_a");
        std::fs::write(a.join("alpha.wav"), wav(48_000, 48_000)).unwrap();
        let b = tmpdir("multi_b");
        std::fs::write(b.join("beta.wav"), wav(48_000, 96_000)).unwrap();

        let lib = Library::scan_many(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(lib.len(), 2, "both sources' tracks aggregated");

        let views = lib.views();
        assert_eq!(views.len(), 2);
        let mut sources = std::collections::BTreeSet::new();
        for v in &views {
            let (src, _) = soul_audio_multicatalog::unpack(v.id);
            sources.insert(src);
            let p = lib.path_of(v.id).expect("path");
            let root = if src == 0 { &a } else { &b };
            assert!(
                p.starts_with(root),
                "{p:?} not under source {src} root {root:?}"
            );
            assert!(p.is_file(), "located file exists: {p:?}");
        }
        assert_eq!(sources.len(), 2, "one track per distinct source");
    }

    #[test]
    fn scan_reads_tags_from_a_tagged_file() {
        let root = tmpdir("tags");
        std::fs::copy(fixture("tagged.flac"), root.join("t.flac")).unwrap();
        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.len(), 1);
        let v = &lib.views()[0];
        assert_eq!(v.title.as_deref(), Some("Soul Test Track"));
        assert_eq!(v.artist.as_deref(), Some("Test Artist"));
        assert_eq!(v.album.as_deref(), Some("Test Album"));
        assert!(v.duration_secs > 0.0, "tagged fixture has a real duration");
    }

    #[test]
    fn scan_reads_id3_tags_from_mp2() {
        let root = tmpdir("mp2tags");
        std::fs::copy(fixture("tagged.mp2"), root.join("t.mp2")).unwrap();
        let v = Library::scan(&root).unwrap().views();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title.as_deref(), Some("Soul Test Track"));
        assert_eq!(v[0].artist.as_deref(), Some("Test Artist"));
    }

    #[test]
    fn scan_reads_id3_tags_from_mp1() {
        let root = tmpdir("mp1tags");
        std::fs::copy(fixture("tagged.mp1"), root.join("t.mp1")).unwrap();
        let v = Library::scan(&root).unwrap().views();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title.as_deref(), Some("Soul Test Track"));
    }

    /// Every representative format fixture scans into exactly one listed track —
    /// the 1:1 guard that the desktop lists every format the decoder supports.
    /// (The `SUPPORTED` extension set itself now lives in `soul-audio-import`.)
    #[test]
    fn every_representative_format_scans_to_one_track() {
        let cases: &[(&str, &str)] = &[
            ("wav", "tone.wav"),
            ("flac", "tone.flac"),
            ("mp3", "tone.mp3"),
            ("mp2", "tone.mp2"),
            ("mp1", "tone.mp1"),
            ("m4a", "tone.m4a"),
            ("ogg", "tone-vorbis.ogg"),
            ("oga", "meta-oggflac.oga"),
            ("opus", "tone.opus"),
            ("aiff", "meta.aiff"),
            ("aif", "tone.aif"),
            ("dsf", "tone.dsf"),
            ("dff", "tone.dff"),
            ("mka", "meta.mka"),
            ("webm", "meta.webm"),
            ("wv", "meta.wv"),
            ("caf", "meta.caf"),
        ];
        for (ext, name) in cases {
            let root = tmpdir(&format!("fmt_{ext}"));
            std::fs::copy(fixture(name), root.join(format!("clip.{ext}")))
                .unwrap_or_else(|e| panic!("copy fixture {name}: {e}"));
            let lib = Library::scan(&root).unwrap();
            assert_eq!(
                lib.len(),
                1,
                "{ext}: fixture {name} did not scan to one track"
            );
        }
    }

    /// `.aac` is NOT in the list above, and must not scan: AAC decode is gated
    /// behind an opt-in feature (patent) this product does not enable, and the
    /// importer refuses to list a file whose decoder was not compiled in.
    ///
    /// Asserted rather than merely omitted — a format that silently started
    /// scanning again would put tracks in the user's library that fail the
    /// moment they press play.
    #[test]
    fn aac_does_not_scan_while_its_decoder_is_gated_off() {
        let root = tmpdir("fmt_aac_gated");
        std::fs::copy(fixture("tone.aac"), root.join("clip.aac")).expect("copy tone.aac");
        let lib = Library::scan(&root).unwrap();
        assert_eq!(
            lib.len(),
            0,
            "a .aac file must not be imported while AAC decode is gated off"
        );
    }

    /// Not importing a file is only half the job: the scan has to SAY it skipped
    /// it, and why. A user whose AAC rips simply never appear cannot tell a
    /// deliberate limitation from a broken scan, which is the actual complaint.
    ///
    /// Asserts the reason reaches the caller too, not just a count — "12 files
    /// skipped" with no cause is barely better than silence, and the two reasons
    /// mean different things to a user: `DecoderDisabled` says a different build
    /// of this app would play them, `UnsupportedFormat` says none would.
    #[test]
    fn an_incremental_scan_reports_what_it_skipped_and_why() {
        let root = tmpdir("skips_reported");
        // One importable track, so this is a normal library that happens to hold
        // files we pass over — not a degenerate all-skipped case.
        std::fs::copy(fixture("tone.wav"), root.join("good.wav")).expect("copy tone.wav");
        // Gated decoder (AAC is behind an opt-in feature — patent).
        std::fs::copy(fixture("tone.aac"), root.join("a.aac")).expect("copy tone.aac");
        std::fs::copy(fixture("tone.aac"), root.join("b.aac")).expect("copy tone.aac");
        // A format nothing here decodes. Contents are irrelevant — the scan
        // classifies by extension before it ever opens the file.
        std::fs::write(root.join("old.wma"), b"not really wma").expect("write wma");
        // Not audio at all: must never be mentioned, or every library with a
        // cover image reports a "skip" the user cannot act on.
        std::fs::write(root.join("cover.jpg"), b"jpeg").expect("write jpg");

        let roots = vec![root.clone()];
        let build = Library::scan_many_incremental(
            &roots,
            &[ScanCache::default()],
            4,
            &AtomicUsize::new(4),
            &mut |_| {},
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(build.library.len(), 1, "only the .wav is importable");

        let (skipped_root, groups) = build
            .skipped
            .iter()
            .find(|(r, _)| r == &root)
            .expect("the root that skipped files is reported");
        assert_eq!(skipped_root, &root);

        let by_ext = |ext: &str| groups.iter().find(|g| g.extension == ext);
        let aac = by_ext("aac").expect("the .aac files are reported");
        assert_eq!(aac.count, 2, "both .aac files counted");
        assert_eq!(
            aac.reason,
            SkipReason::DecoderDisabled,
            "AAC is skipped because its decoder is gated off in this build, \
             not because the format is unknown"
        );
        let wma = by_ext("wma").expect("the .wma file is reported");
        assert_eq!(wma.count, 1);
        assert_eq!(wma.reason, SkipReason::UnsupportedFormat);
        assert!(
            by_ext("jpg").is_none(),
            "a cover image is not audio and must never be reported as a skip"
        );
    }

    /// A minimal FLAC container with a VORBIS_COMMENT (title/artist/album) and a
    /// PICTURE block (front cover, 1x1 jpeg stub) — self-contained so the cover
    /// path is exercised without depending on a binary fixture carrying art.
    fn flac_with_art(title: &str, artist: &str, album: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"fLaC");
        v.push(0x00); // STREAMINFO (type 0), not last
        v.extend_from_slice(&[0, 0, 34]);
        let mut si = [0u8; 34];
        let (sr, ch, bps, total): (u32, u32, u32, u64) = (44_100, 2, 16, 44_100);
        si[10] = (sr >> 12) as u8;
        si[11] = (sr >> 4) as u8;
        si[12] =
            (((sr & 0xF) as u8) << 4) | ((((ch - 1) & 0x7) as u8) << 1) | (((bps - 1) >> 4) as u8);
        si[13] = ((((bps - 1) & 0xF) as u8) << 4) | (((total >> 32) & 0xF) as u8);
        si[14] = (total >> 24) as u8;
        si[15] = (total >> 16) as u8;
        si[16] = (total >> 8) as u8;
        si[17] = total as u8;
        v.extend_from_slice(&si);
        let comments = [
            format!("TITLE={title}"),
            format!("ARTIST={artist}"),
            format!("ALBUM={album}"),
        ];
        let mut vc = Vec::new();
        vc.extend_from_slice(&6u32.to_le_bytes());
        vc.extend_from_slice(b"vendor");
        vc.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for c in &comments {
            vc.extend_from_slice(&(c.len() as u32).to_le_bytes());
            vc.extend_from_slice(c.as_bytes());
        }
        v.push(0x04); // VORBIS_COMMENT (type 4), not last
        v.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
        v.extend_from_slice(&vc);
        let data = b"\xFF\xD8\xFF\xEE";
        let mut pic = Vec::new();
        pic.extend_from_slice(&3u32.to_be_bytes()); // type 3 = front cover
        let mime = b"image/jpeg";
        pic.extend_from_slice(&(mime.len() as u32).to_be_bytes());
        pic.extend_from_slice(mime);
        pic.extend_from_slice(&0u32.to_be_bytes()); // desc len
        pic.extend_from_slice(&1u32.to_be_bytes()); // width
        pic.extend_from_slice(&1u32.to_be_bytes()); // height
        pic.extend_from_slice(&24u32.to_be_bytes()); // depth
        pic.extend_from_slice(&0u32.to_be_bytes()); // colors
        pic.extend_from_slice(&(data.len() as u32).to_be_bytes());
        pic.extend_from_slice(data);
        v.push(0x86); // last, type 6 (PICTURE)
        v.extend_from_slice(&(pic.len() as u32).to_be_bytes()[1..]);
        v.extend_from_slice(&pic);
        v
    }

    /// A minimal FLAC carrying a VORBIS_COMMENT with the given `comments`
    /// (`"KEY=value"`) and no audio frames — enough for the scan/bake tag path
    /// (title/genre/…). Reuses the STREAMINFO shape of [`flac_with_art`] but makes
    /// VORBIS_COMMENT the last metadata block and omits the PICTURE.
    fn flac_with_comments(comments: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"fLaC");
        v.push(0x00); // STREAMINFO (type 0), not last
        v.extend_from_slice(&[0, 0, 34]);
        let mut si = [0u8; 34];
        let (sr, ch, bps, total): (u32, u32, u32, u64) = (44_100, 2, 16, 44_100);
        si[10] = (sr >> 12) as u8;
        si[11] = (sr >> 4) as u8;
        si[12] =
            (((sr & 0xF) as u8) << 4) | ((((ch - 1) & 0x7) as u8) << 1) | (((bps - 1) >> 4) as u8);
        si[13] = ((((bps - 1) & 0xF) as u8) << 4) | (((total >> 32) & 0xF) as u8);
        si[14] = (total >> 24) as u8;
        si[15] = (total >> 16) as u8;
        si[16] = (total >> 8) as u8;
        si[17] = total as u8;
        v.extend_from_slice(&si);
        let mut vc = Vec::new();
        vc.extend_from_slice(&6u32.to_le_bytes());
        vc.extend_from_slice(b"vendor");
        vc.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for c in comments {
            vc.extend_from_slice(&(c.len() as u32).to_le_bytes());
            vc.extend_from_slice(c.as_bytes());
        }
        v.push(0x84); // last block, type 4 (VORBIS_COMMENT)
        v.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
        v.extend_from_slice(&vc);
        v
    }

    /// `views_by(Genre)` projects each track's genre and the Genre axis clusters
    /// tracks by genre bucket. An untagged file keeps the real "Unknown Genre"
    /// bucket rather than being dropped to `None` (the artist/album sentinels are).
    #[test]
    fn views_by_genre_carries_genre_and_groups_by_genre_bucket() {
        let root = tmpdir("genre");
        // Tagged genres across tracks; sorted filenames give ids a=0,b=1,c=2.
        std::fs::write(
            root.join("a.flac"),
            flac_with_comments(&["TITLE=A", "GENRE=Rock"]),
        )
        .unwrap();
        std::fs::write(
            root.join("b.flac"),
            flac_with_comments(&["TITLE=B", "GENRE=Ambient"]),
        )
        .unwrap();
        std::fs::write(
            root.join("c.flac"),
            flac_with_comments(&["TITLE=C", "GENRE=Rock"]),
        )
        .unwrap();
        // An untagged file → the kept "Unknown Genre" bucket.
        std::fs::write(root.join("d.wav"), wav(48_000, 48_000)).unwrap();

        let lib = Library::scan(&root).unwrap();
        let v = lib.views_by(BrowseAxis::Genre);
        assert_eq!(v.len(), 4);
        // Every track carries a genre bucket — none are `None`.
        assert!(
            v.iter().all(|t| t.genre.is_some()),
            "every track carries a genre bucket"
        );
        assert!(
            v.iter()
                .any(|t| t.genre.as_deref() == Some("Unknown Genre")),
            "an untagged file keeps the real Unknown Genre bucket, not None"
        );
        // The Genre axis orders by genre name and keeps each genre's tracks
        // contiguous: Ambient, then both Rock tracks, then Unknown Genre.
        let genres: Vec<&str> = v.iter().map(|t| t.genre.as_deref().unwrap()).collect();
        assert_eq!(genres, vec!["Ambient", "Rock", "Rock", "Unknown Genre"]);
    }

    /// `format_label` maps a resolved file name's extension to the uppercase
    /// label the Tracks table's FORMAT badge shows: known aliases normalize to
    /// the old app's label, an unlisted-but-real extension just uppercases
    /// itself, and a name with no extension carries no format at all.
    #[test]
    fn format_label_maps_extensions_to_uppercase_labels() {
        assert_eq!(format_label("track.flac").as_deref(), Some("FLAC"));
        assert_eq!(format_label("TRACK.FLAC").as_deref(), Some("FLAC"));
        assert_eq!(format_label("track.mp3").as_deref(), Some("MP3"));
        assert_eq!(format_label("track.wav").as_deref(), Some("WAV"));
        assert_eq!(format_label("track.m4a").as_deref(), Some("M4A"));
        assert_eq!(format_label("track.aac").as_deref(), Some("AAC"));
        assert_eq!(format_label("track.ogg").as_deref(), Some("OGG"));
        assert_eq!(format_label("track.oga").as_deref(), Some("OGG"));
        assert_eq!(format_label("track.opus").as_deref(), Some("OPUS"));
        assert_eq!(format_label("track.aif").as_deref(), Some("AIFF"));
        assert_eq!(format_label("track.aiff").as_deref(), Some("AIFF"));
        assert_eq!(format_label("track.dsf").as_deref(), Some("DSF"));
        assert_eq!(format_label("track.dff").as_deref(), Some("DFF"));
        assert_eq!(format_label("track.wv").as_deref(), Some("WV"));
        assert_eq!(format_label("track.mp2").as_deref(), Some("MP2"));
        // Unknown-but-real extension: falls back to the uppercased extension.
        assert_eq!(format_label("track.xyz").as_deref(), Some("XYZ"));
        // No extension at all: no format.
        assert_eq!(format_label("track"), None);
        assert_eq!(format_label("README"), None);
    }

    /// `views_by` derives each track's `format` from its resolved file name's
    /// extension (the FORMAT badge data), scanned end to end through the real
    /// import -> bake -> `IndexReader` -> projection chain (not just the pure
    /// `format_label` helper above).
    #[test]
    fn views_by_populates_format_from_file_extension() {
        let root = tmpdir("format");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::copy(fixture("tagged.flac"), root.join("b.flac")).unwrap();

        let lib = Library::scan(&root).unwrap();
        let views = lib.views_by(BrowseAxis::ArtistAlbumTrack);
        assert_eq!(views.len(), 2);

        let wav_view = views
            .iter()
            .find(|v| lib.path_of(v.id).unwrap().ends_with("a.wav"))
            .expect("the wav track is present");
        assert_eq!(wav_view.format.as_deref(), Some("WAV"));

        let flac_view = views
            .iter()
            .find(|v| lib.path_of(v.id).unwrap().ends_with("b.flac"))
            .expect("the flac track is present");
        assert_eq!(flac_view.format.as_deref(), Some("FLAC"));
    }

    /// `views_by` projects the owning album's packed original/release dates onto
    /// each `TrackView` — the audiophile original-release year plus the tagged
    /// edition's release year, both year-only (month/day unknown).
    #[test]
    fn track_view_carries_album_original_and_release_dates() {
        let root = tmpdir("dates");
        std::fs::write(
            root.join("a.flac"),
            flac_with_comments(&[
                "TITLE=A",
                "ALBUM=Test Album",
                "ARTIST=Test Artist",
                "ORIGINALDATE=1973",
                "DATE=2015",
            ]),
        )
        .unwrap();

        let lib = Library::scan(&root).unwrap();
        let views = lib.views_by(BrowseAxis::ArtistAlbumTrack);
        let t = views
            .iter()
            .find(|t| t.album.as_deref() == Some("Test Album"))
            .unwrap();
        // 1973 packed as a year-only date = 1973 << 9.
        assert_eq!(t.original_date, (1973u32) << 9);
        assert_eq!(t.release_date, (2015u32) << 9);
    }

    /// End-to-end: a realistic reissue-shaped library (one artist, several albums
    /// with a mix of date tags) scanned through the WHOLE chain -- file tags ->
    /// `soul-audio-import` parse -> `.soulidx` v5 bake -> `IndexReader` -> the
    /// `album_dates` projection -> `TrackView`. Proves per-album dates survive at
    /// full precision AND that the artist's albums come out ordered chronologically
    /// by the ORIGINAL date (core's `year_at` default), with undated albums last.
    #[test]
    fn reissue_library_scans_to_dated_tracks_in_original_year_order() {
        let root = tmpdir("reissue_e2e");
        // One track per album, same artist. Written in a scrambled filename order
        // so the result order can only come from the baked ArtistAlbumTrack axis,
        // not from the filesystem.
        let write = |file: &str, comments: &[&str]| {
            std::fs::write(root.join(file), flac_with_comments(comments)).unwrap();
        };
        write(
            "z-later.flac",
            &[
                "TITLE=L",
                "ARTIST=Aja Band",
                "ALBUM=Later",
                "ORIGINALDATE=1980",
                "DATE=1980",
            ],
        );
        write(
            "a-peak.flac",
            &[
                "TITLE=P",
                "ARTIST=Aja Band",
                "ALBUM=Peak",
                "ORIGINALDATE=1977-09-23",
                "DATE=2011",
            ],
        );
        write(
            "m-early.flac",
            &[
                "TITLE=E",
                "ARTIST=Aja Band",
                "ALBUM=Early Works",
                "ORIGINALDATE=1972",
                "DATE=2010",
            ],
        );
        // Release date only (no ORIGINALDATE): original stays unknown (0).
        write(
            "q-relonly.flac",
            &[
                "TITLE=R",
                "ARTIST=Aja Band",
                "ALBUM=ReleaseOnly",
                "DATE=1995",
            ],
        );
        // No date tags at all: both dates unknown (0).
        write(
            "b-untag.flac",
            &["TITLE=U", "ARTIST=Aja Band", "ALBUM=Untagged"],
        );

        let views = Library::scan(&root)
            .unwrap()
            .views_by(BrowseAxis::ArtistAlbumTrack);
        assert_eq!(views.len(), 5);

        // One track per album, so the view order IS the album order. The dated
        // albums sort by original year (1972 < 1977 < 1980); the two undated-original
        // albums trail (sort_key u32::MAX), tiebroken by album name (ReleaseOnly < Untagged).
        let order: Vec<&str> = views.iter().map(|t| t.album.as_deref().unwrap()).collect();
        assert_eq!(
            order,
            vec!["Early Works", "Peak", "Later", "ReleaseOnly", "Untagged"],
            "artist's albums come out chronological by ORIGINAL date, undated last by title"
        );

        // Per-album packed dates survived the scan->bake->project chain.
        let dates = |album: &str| -> (u32, u32) {
            let t = views
                .iter()
                .find(|t| t.album.as_deref() == Some(album))
                .unwrap();
            (t.original_date, t.release_date)
        };
        assert_eq!(dates("Early Works"), (1972u32 << 9, 2010u32 << 9));
        // Full YYYY-MM-DD precision is preserved on the original date.
        assert_eq!(
            dates("Peak"),
            ((1977u32 << 9) | (9u32 << 5) | 23, 2011u32 << 9)
        );
        assert_eq!(dates("Later"), (1980u32 << 9, 1980u32 << 9));
        assert_eq!(dates("ReleaseOnly"), (0, 1995u32 << 9)); // no ORIGINALDATE -> original unknown
        assert_eq!(dates("Untagged"), (0, 0)); // no date tags -> both unknown
    }

    #[test]
    fn embedded_cover_resolves_via_cover_bytes() {
        let root = tmpdir("art");
        let path = root.join("cover.flac");
        std::fs::write(&path, flac_with_art("Artful", "Artist", "Album")).unwrap();
        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.len(), 1);
        assert_eq!(lib.views()[0].title.as_deref(), Some("Artful"));
        assert!(
            crate::cover_bytes(&lib.path_of(0).unwrap()).is_some(),
            "embedded PICTURE block must resolve to a cover"
        );
    }

    #[test]
    fn scan_survives_a_corrupt_supported_file() {
        let root = tmpdir("corrupt");
        std::fs::write(root.join("ok.wav"), wav(48_000, 48_000)).unwrap(); // 1.0 s
        std::fs::write(root.join("bad.flac"), b"this is not a flac").unwrap();
        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.len(), 2, "corrupt file still listed");
        // The corrupt file yields no duration; the good one does.
        let durs: Vec<_> = (0..2).map(|i| lib.duration_of(i)).collect();
        assert!(
            durs.iter().any(Option::is_none),
            "corrupt file has no duration"
        );
        assert!(
            durs.iter().flatten().any(|&d| (d - 1.0).abs() < 0.05),
            "good file keeps its duration"
        );
    }

    #[test]
    fn scan_errors_when_root_is_missing() {
        let missing = std::env::temp_dir().join("sas_lib_definitely_missing");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(Library::scan(&missing).is_err());
    }

    #[test]
    fn source_counts_reports_tracks_per_root() {
        // Two roots with distinct, known track counts; source_counts() must
        // report each root's own count, in scan order, alongside the total.
        let a = tmpdir("counts_a");
        std::fs::write(a.join("a1.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(a.join("a2.wav"), wav(48_000, 48_000)).unwrap();
        let b = tmpdir("counts_b");
        std::fs::write(b.join("b1.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(b.join("b2.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(b.join("b3.wav"), wav(48_000, 48_000)).unwrap();

        let lib = Library::scan_many(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(lib.len(), 5);
        assert_eq!(lib.source_counts(), vec![(a, 2), (b, 3)]);
    }

    #[test]
    fn scan_bakes_loudness_from_the_provided_map() {
        // The Tier-2 injection: a scan stamps per-track loudness (from the host's live map)
        // into the baked `.soulidx`, and `views()` reads it back on `TrackView.loudness`.
        let root = tmpdir("loud_bake");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("b.wav"), wav(48_000, 24_000)).unwrap();
        let roots = vec![root.clone()];

        let la = TrackLoudness {
            integrated_lufs: -14.0,
            lra: 7.5,
            true_peak_dbtp: -1.0,
            plr: 13.0,
            dr: 11.0,
        };
        // Provide loudness for a.wav only; b.wav stays unanalyzed.
        let mut map = HashMap::new();
        map.insert(root.join("a.wav"), la);

        let build = Library::scan_many_incremental(
            &roots,
            &[ScanCache::default()],
            4,
            &AtomicUsize::new(4),
            &mut |_| {},
            &map,
        )
        .unwrap();

        let views = build.library.views();
        let view_for = |name: &str| {
            views
                .iter()
                .find(|v| {
                    build
                        .library
                        .path_of(v.id)
                        .is_some_and(|p| p.ends_with(name))
                })
                .unwrap_or_else(|| panic!("no view for {name}"))
        };
        assert_eq!(
            view_for("a.wav").loudness,
            Some(la),
            "a.wav's loudness baked + read back"
        );
        assert_eq!(
            view_for("b.wav").loudness,
            None,
            "b.wav unanalyzed reads None"
        );
    }

    #[test]
    fn incremental_scan_matches_full_scan_then_warm_reflects_add_and_remove() {
        let root = tmpdir("incr");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("b.wav"), wav(48_000, 24_000)).unwrap();
        let roots = vec![root.clone()];

        // Cold incremental scan (empty prior) lists the same tracks as a full scan.
        let titles = |l: &Library| l.views().into_iter().map(|v| v.id).collect::<Vec<_>>();
        let build = Library::scan_many_incremental(
            &roots,
            &[ScanCache::default()],
            4,
            &AtomicUsize::new(4),
            &mut |_| {},
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(build.library.len(), 2);
        assert_eq!(
            titles(&build.library),
            titles(&Library::scan_many(&roots).unwrap()),
            "incremental cold scan lists the same tracks as a full scan"
        );
        // The returned cache is warm (one entry per file) and nothing was removed.
        assert_eq!(build.caches.len(), 1);
        assert_eq!(build.caches[0].len(), 2, "cache holds both files");
        assert!(build.removed.is_empty());

        // Warm rescan reusing the prior cache: an added file is reflected.
        std::fs::write(root.join("c.wav"), wav(48_000, 48_000)).unwrap();
        let build2 = Library::scan_many_incremental(
            &roots,
            &build.caches,
            4,
            &AtomicUsize::new(4),
            &mut |_| {},
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(build2.library.len(), 3, "added file appears");
        assert!(build2.removed.is_empty());

        // Deletions are reported per root (an available root — genuine removals).
        std::fs::remove_file(root.join("a.wav")).unwrap();
        std::fs::remove_file(root.join("b.wav")).unwrap();
        let build3 = Library::scan_many_incremental(
            &roots,
            &build2.caches,
            4,
            &AtomicUsize::new(4),
            &mut |_| {},
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(build3.library.len(), 1, "only c.wav remains");
        assert_eq!(build3.removed.len(), 1, "one root reported deletions");
        assert_eq!(build3.removed[0].0, root);
        assert_eq!(build3.removed[0].1.len(), 2, "two files removed");
    }

    #[test]
    fn from_source_bytes_rebuilds_and_scoped_reuse_matches_a_full_rescan() {
        // The smart-watcher primitive: because each root bakes INDEPENDENTLY into its
        // own reader (no cross-root interning), an unaffected root's baked bytes can be
        // reused verbatim instead of re-scanned — the result is byte-identical to a
        // full rescan.
        let a = tmpdir("scoped_a");
        std::fs::write(a.join("a1.wav"), wav(48_000, 48_000)).unwrap();
        let b = tmpdir("scoped_b");
        std::fs::write(b.join("b1.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(b.join("b2.wav"), wav(48_000, 24_000)).unwrap();
        let roots = vec![a.clone(), b.clone()];
        let ids = |l: &Library| l.views().into_iter().map(|v| v.id).collect::<Vec<_>>();
        let cold = || [ScanCache::default(), ScanCache::default()];

        // A scan carries back the baked bytes, parallel to `roots`.
        let full = Library::scan_many_incremental(
            &roots,
            &cold(),
            4,
            &AtomicUsize::new(4),
            &mut |_| {},
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(full.bytes.len(), 2, "one baked index per root");

        // `from_source_bytes` rebuilds the SAME library with no scan.
        let rebuilt = Library::from_source_bytes(&roots, full.bytes.clone());
        assert_eq!(
            ids(&rebuilt),
            ids(&full.library),
            "rebuilt-from-bytes == scanned"
        );
        assert_eq!(rebuilt.source_counts(), full.library.source_counts());

        // Change root A only; rescan JUST A and reuse B's cached bytes → identical to
        // a full rescan of both roots.
        std::fs::write(a.join("a2.wav"), wav(48_000, 48_000)).unwrap();
        let only_a = Library::scan_many_incremental(
            std::slice::from_ref(&a),
            &[full.caches[0].clone()],
            4,
            &AtomicUsize::new(4),
            &mut |_| {},
            &HashMap::new(),
        )
        .unwrap();
        let merged = vec![only_a.bytes[0].clone(), full.bytes[1].clone()];
        let scoped = Library::from_source_bytes(&roots, merged);

        let full_rescan = Library::scan_many_incremental(
            &roots,
            &full.caches,
            4,
            &AtomicUsize::new(4),
            &mut |_| {},
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            ids(&scoped),
            ids(&full_rescan.library),
            "scoped reuse of B == a full rescan"
        );
        assert_eq!(
            scoped.source_counts(),
            full_rescan.library.source_counts(),
            "per-source counts match the full rescan"
        );
    }

    #[test]
    fn scan_many_incremental_reports_determinate_progress() {
        // A cold scan of three files emits Extracting progress whose `done` climbs
        // monotonically to `total` == the file count — the determinate signal the
        // titlebar pill renders.
        let root = tmpdir("progress");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("b.wav"), wav(48_000, 48_000)).unwrap();
        std::fs::write(root.join("c.wav"), wav(48_000, 48_000)).unwrap();
        let roots = vec![root.clone()];

        let mut ticks: Vec<ScanProgress> = Vec::new();
        let build = Library::scan_many_incremental(
            &roots,
            &[ScanCache::default()],
            4,
            &AtomicUsize::new(4),
            &mut |p| ticks.push(p),
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(build.library.len(), 3);

        let extracting: Vec<&ScanProgress> = ticks
            .iter()
            .filter(|p| p.phase == ScanPhase::Extracting)
            .collect();
        assert!(
            !extracting.is_empty(),
            "a cold scan reports Extracting progress"
        );
        // `done` never exceeds `total`, and `total` is the file count throughout.
        for p in &extracting {
            assert!(p.done <= p.total, "done {} <= total {}", p.done, p.total);
            assert_eq!(p.total, 3, "total is the extract count");
        }
        // The final Extracting tick completes (done == total).
        assert_eq!(
            extracting.last().map(|p| p.done),
            Some(3),
            "extraction finishes at done == total"
        );
    }

    #[test]
    fn library_as_a_trait_object_resolves_the_same_path() {
        // Proves `TrackSource` is a usable trait object over `Library`: resolving
        // through `dyn TrackSource` must agree with the inherent `Library::path_of`.
        let root = tmpdir("tracksource");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();

        let lib = Library::scan_many(std::slice::from_ref(&root)).unwrap();
        let expected = Library::path_of(&lib, 0);
        let ts: std::sync::Arc<dyn TrackSource> = std::sync::Arc::new(lib);
        assert_eq!(ts.path_of(0), expected);
        assert_eq!(ts.path_of(9), None);
    }

    /// The default `TrackSource::open_decoder` (what `Library` relies on):
    /// resolves `path_of` then opens the file with `open_track`, equivalent to
    /// the loader's old direct `open_track(&path_of(id))` call before the
    /// streaming split (`soul_player_host::remote::RemoteLibrary` overrides
    /// this method instead, to stream a remote track over HTTP Range).
    #[test]
    fn open_decoder_opens_and_decodes_a_local_track() {
        use soul_audio_core::Decoder;

        let root = tmpdir("open_decoder");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap(); // 1.0 s
        let lib = Library::scan(&root).unwrap();

        // `OwnedDecoder` is large by value (the boxed `AnyDecoder`) — a
        // big-stack thread, the same precaution the actor's loader thread and
        // other open_track-decoding tests take for debug-build move frames.
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut decoder = lib
                    .open_decoder(0)
                    .expect("open_decoder should resolve track 0 and open it");
                let mut buf = [0i32; 4096];
                let n = decoder
                    .decode_into(&mut buf)
                    .expect("decode the first block");
                assert!(n > 0, "expected a non-empty decoded block");
            })
            .expect("spawn decode thread")
            .join()
            .expect("decode thread panicked");
    }

    #[test]
    fn open_decoder_of_an_unknown_id_errors() {
        let root = tmpdir("open_decoder_missing");
        std::fs::write(root.join("a.wav"), wav(48_000, 48_000)).unwrap();
        let lib = Library::scan(&root).unwrap();
        assert!(lib.open_decoder(9).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn scan_stops_at_a_symlink_cycle() {
        use std::os::unix::fs::symlink;
        let root = tmpdir("cycle");
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.wav"), wav(48_000, 48_000)).unwrap();
        symlink(&root, root.join("sub/loop")).unwrap();
        let lib = Library::scan(&root).unwrap();
        assert_eq!(lib.len(), 1);
        assert!(lib.path_of(0).unwrap().ends_with("a.wav"));
    }
}
