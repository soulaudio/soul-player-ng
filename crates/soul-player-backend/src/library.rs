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

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use embedded_io_adapters::std::FromStd;
use soul_audio_catalog::{BrowseAxis, Catalog, StrRef};
use soul_audio_import::{ScanCache, ScanProgress};
use soul_audio_index::IndexReader;
use soul_audio_multicatalog::{MultiCatalog, unpack};

use crate::types::TrackView;

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
    ) -> anyhow::Result<IncrementalBuild> {
        let max_threads = max_threads.max(1);
        let mut members: Vec<Box<dyn Catalog + Send>> = Vec::with_capacity(roots.len());
        let mut counts = Vec::with_capacity(roots.len());
        let mut caches = Vec::with_capacity(roots.len());
        let mut all_bytes = Vec::with_capacity(roots.len());
        let mut removed = Vec::new();
        for (i, root) in roots.iter().enumerate() {
            let prior = priors.get(i).cloned().unwrap_or_default();
            let outcome = soul_audio_import::scan_incremental_throttled(
                root,
                &prior,
                max_threads,
                target,
                progress,
            )?;
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
        let cat = self.cat();
        (0..cat.order_len(axis))
            .filter_map(|i| {
                let id = cat.order(axis, i).ok()?;
                let t = cat.track(id).ok()?;
                Some(TrackView {
                    id,
                    title: resolve(&cat, t.title),
                    artist: real_name(artist_name(&cat, t.artist)),
                    album: real_name(album_title(&cat, t.album)),
                    // Genre keeps its raw value — "Unknown Genre" is a real bucket,
                    // NOT filtered through `real_name` like artist/album sentinels.
                    genre: genre_name(&cat, t.genre),
                    duration_secs: f64::from(t.duration_ms) / 1000.0,
                    // The backend/local catalog is always local — the host
                    // aggregate stamps `true` for a remote source's tracks.
                    remote: false,
                })
            })
            .collect()
    }
}

/// Resolves a track id to a local file path for the loader. `Library` resolves
/// it locally; a host-side adapter can resolve remote ids by fetching them.
pub trait TrackSource: Send + Sync {
    fn path_of(&self, id: u32) -> Option<std::path::PathBuf>;

    /// The tag-derived duration (seconds), used by the pump's Tick fallback when
    /// the decoder itself can't report a length. Defaults to "unknown" so an
    /// implementor only needs this when it has a cheaper-than-decoding answer.
    fn duration_of(&self, _id: u32) -> Option<f64> {
        None
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
    use soul_audio_import::ScanPhase;

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
            ("aac", "tone.aac"),
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
        let full =
            Library::scan_many_incremental(&roots, &cold(), 4, &AtomicUsize::new(4), &mut |_| {})
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
