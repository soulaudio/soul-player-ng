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

use embedded_io_adapters::std::FromStd;
use soul_audio_catalog::{BrowseAxis, Catalog, StrRef};
use soul_audio_index::IndexReader;
use soul_audio_multicatalog::{MultiCatalog, unpack};

use crate::types::TrackView;

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
        for root in roots {
            let bytes = soul_audio_import::bake(root)?;
            let reader: MemReader = IndexReader::new(FromStd::new(Cursor::new(bytes)));
            members.push(Box::new(reader));
        }
        Ok(Library {
            roots: roots.to_vec(),
            catalog: Mutex::new(MultiCatalog::new(members)),
        })
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

    /// The absolute file path for track `id`: `roots[source] / folder.path /
    /// file_name`. The compound id's source selects the physical root; the
    /// `/`-separated, root-relative `folder.path` is re-split into native
    /// components.
    #[must_use]
    pub fn path_of(&self, id: u32) -> Option<PathBuf> {
        let (source, _) = unpack(id);
        let root = self.roots.get(source as usize)?;
        let cat = self.cat();
        let t = cat.track(id).ok()?;
        let folder = cat.folder(t.folder).ok()?;
        let rel = resolve(&cat, folder.path)?;
        let name = resolve(&cat, t.file_name)?;
        let mut p = root.clone();
        if !rel.is_empty() {
            for seg in rel.split('/') {
                p.push(seg);
            }
        }
        p.push(&name);
        Some(p)
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
                    duration_secs: f64::from(t.duration_ms) / 1000.0,
                })
            })
            .collect()
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
