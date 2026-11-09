//! Local music library, backed by the **shared** data layer.
//!
//! The recursive folder scan + tag reading that used to live here now lives in
//! [`soul_audio_import`] (shared 1:1 with the e-ink DAP through
//! `soul-audio-catalog`'s model). This module is the thin desktop wrapper the
//! actor and the Tauri commands already consume: [`Library::scan`] produces the
//! shared in-memory catalog, and [`Library::path_of`] / [`Library::duration_of`]
//! / [`Library::views`] read it. Folder-isolated albums/artists come for free
//! from the shared importer; album/artist/folder *browse* is layered on next.

use std::path::{Path, PathBuf};

use soul_audio_index::build::CatalogInput;

use crate::types::TrackView;

/// An in-memory library: the shared scanned catalog, plus the root it was
/// scanned from (needed to turn a track's `/`-separated folder path + file name
/// back into an absolute path for the decoder).
pub struct Library {
    root: PathBuf,
    input: CatalogInput,
}

impl Library {
    /// Scan `root` recursively via the shared importer — folder-isolation, tag
    /// reading, and every supported format — into an in-memory catalog. Track
    /// ids are the scan order (stable for the session), matching the old flat
    /// model so the actor/UI are unaffected. Robustness (unreadable tags degrade,
    /// unreadable subdirs are skipped, symlink cycles terminate, only an
    /// unreadable `root` errors) is the importer's, shared with the DAP.
    pub fn scan(root: &Path) -> anyhow::Result<Library> {
        let input = soul_audio_import::scan(root)?;
        Ok(Library {
            root: root.to_owned(),
            input,
        })
    }

    /// Number of scanned tracks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.input.tracks.len()
    }

    /// Whether the scan found no tracks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.input.tracks.is_empty()
    }

    /// The absolute file path for track `id`: `root / folder.path / file_name`.
    /// `folder.path` is the importer's `/`-separated, root-relative form, so it
    /// is re-split into native components here.
    #[must_use]
    pub fn path_of(&self, id: u32) -> Option<PathBuf> {
        let t = self.input.tracks.get(id as usize)?;
        let folder = self.input.folders.get(t.folder as usize)?;
        let mut p = self.root.clone();
        if !folder.path.is_empty() {
            for seg in folder.path.split('/') {
                p.push(seg);
            }
        }
        p.push(&t.file_name);
        Some(p)
    }

    /// The tag-derived duration (seconds) for `id`, if known (> 0). Used to fill
    /// a Tick when the decoder can't report a length, keeping the progress bar
    /// consistent with the track list.
    #[must_use]
    pub fn duration_of(&self, id: u32) -> Option<f64> {
        let t = self.input.tracks.get(id as usize)?;
        (t.duration_ms > 0).then(|| f64::from(t.duration_ms) / 1000.0)
    }

    /// The UI-facing view of every track (no paths). The importer already applies
    /// the file-name fallback for a missing title, so `title` is always present.
    #[must_use]
    pub fn views(&self) -> Vec<TrackView> {
        self.input
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| TrackView {
                id: i as u32,
                title: Some(t.title.clone()),
                artist: real_name(self.input.artists.get(t.artist as usize)),
                album: real_name(self.input.albums.get(t.album as usize).map(|a| &a.title)),
                duration_secs: f64::from(t.duration_ms) / 1000.0,
            })
            .collect()
    }
}

/// Map the importer's `"Unknown Artist"` / `"Unknown Album"` placeholders back to
/// `None`, preserving the UI's "blank, not the word Unknown" presentation for an
/// untagged file.
fn real_name(s: Option<&String>) -> Option<String> {
    match s.map(String::as_str) {
        None | Some("Unknown Artist" | "Unknown Album") => None,
        Some(name) => Some(name.to_owned()),
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
