//! `CoreIndex`: the [`IndexPort`] adapter over the backend [`Library`] scanner.
//!
//! Reuses `soul-player-backend::Library` — which itself reuses the shared
//! `soul-audio-import`/`soul-audio-index` scan + bake pipeline, the SAME code
//! the desktop app's library scan runs — instead of reimplementing any file
//! walking or tag reading here. This module only adds: capturing the per-root
//! baked `.soulidx` bytes so they can be shipped to a remote client, and a
//! trivial framing format to bundle several of them into one blob.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use anyhow::Context;
use soul_player_backend::{IncrementalBuild, Library, ScanCache};

use crate::domain::IndexVersion;
use crate::domain::ports::IndexPort;

/// [`IndexPort`] over the backend [`Library`]: scans a set of root folders
/// with the shared importer, keeps the aggregated library plus each root's
/// baked `.soulidx` bytes (so [`index_bytes`](IndexPort::index_bytes) can hand
/// a client the exact bytes it needs to rebuild the same catalog), and tracks
/// a version counter bumped on rescan.
pub struct CoreIndex {
    library: Library,
    /// Baked `.soulidx` bytes, one per root, parallel to `roots` — the
    /// payload framed by [`index_bytes`](IndexPort::index_bytes).
    source_bytes: Vec<Vec<u8>>,
    #[allow(dead_code)] // kept for a future `rescan`; not read yet.
    roots: Vec<PathBuf>,
    version: AtomicU64,
}

impl CoreIndex {
    /// Scan `roots` with the shared library scanner and capture the baked
    /// per-root bytes alongside it.
    ///
    /// Uses [`Library::scan_many_incremental`] with cold (empty) priors
    /// rather than [`Library::scan_many`]: functionally both perform a full
    /// scan, but only the incremental entry point hands back the baked bytes
    /// per root (`scan_many` keeps only a reader built from them, discarding
    /// the bytes) — exactly the payload `index_bytes` needs to serve a
    /// client. Version starts at 1.
    pub fn scan(roots: Vec<PathBuf>) -> anyhow::Result<Self> {
        let priors = vec![ScanCache::default(); roots.len()];
        let threads = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4);
        let target = AtomicUsize::new(threads);
        // The server has no loudness analysis (no playback actor), so bake without it.
        let no_loudness = std::collections::HashMap::new();
        let IncrementalBuild { library, bytes, .. } = Library::scan_many_incremental(
            &roots,
            &priors,
            threads,
            &target,
            &mut |_| {},
            &no_loudness,
        )
        .context("scanning configured roots")?;
        Ok(Self {
            library,
            source_bytes: bytes,
            roots,
            version: AtomicU64::new(1),
        })
    }
}

impl IndexPort for CoreIndex {
    fn version(&self) -> IndexVersion {
        IndexVersion(self.version.load(Ordering::SeqCst))
    }

    fn index_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(frame_index(&self.source_bytes))
    }

    fn locate(&self, track_id: u32) -> Option<PathBuf> {
        self.library.path_of(track_id)
    }
}

/// Frame baked `.soulidx` blobs into one bundle: `[u32 LE n][per blob: u32 LE
/// length, that many bytes]` — a trivial concat a client can re-split without
/// depending on this crate (see [`deframe_index`], the reference
/// implementation of the same six lines).
#[must_use]
pub fn frame_index(blobs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
    for blob in blobs {
        out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        out.extend_from_slice(blob);
    }
    out
}

/// The inverse of [`frame_index`]: split a framed bundle back into its blobs.
pub fn deframe_index(bytes: &[u8]) -> anyhow::Result<Vec<Vec<u8>>> {
    fn read_u32(bytes: &[u8], pos: &mut usize) -> anyhow::Result<u32> {
        let slice = bytes
            .get(*pos..*pos + 4)
            .context("framed index truncated: expected a u32 length")?;
        *pos += 4;
        Ok(u32::from_le_bytes(
            slice.try_into().expect("slice is 4 bytes"),
        ))
    }

    let mut pos = 0usize;
    let n = read_u32(bytes, &mut pos)? as usize;
    let mut blobs = Vec::with_capacity(n);
    for _ in 0..n {
        let len = read_u32(bytes, &mut pos)? as usize;
        let blob = bytes
            .get(pos..pos + len)
            .context("framed index truncated: expected a blob")?
            .to_vec();
        pos += len;
        blobs.push(blob);
    }
    Ok(blobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path to the committed `testdata/` fixtures (relative to this crate's
    /// manifest dir, two levels up to the repo root).
    fn testdata_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
    }

    #[test]
    fn frame_index_round_trips_on_its_own() {
        let blobs = vec![b"hello".to_vec(), Vec::new(), b"a bit longer blob".to_vec()];

        let framed = frame_index(&blobs);
        let back = deframe_index(&framed).unwrap();

        assert_eq!(back, blobs);
    }

    #[test]
    fn deframe_index_rejects_truncated_bytes() {
        // A length-4 count header claiming one blob, but no blob bytes follow.
        let truncated = 1u32.to_le_bytes().to_vec();
        assert!(deframe_index(&truncated).is_err());
    }

    #[test]
    fn scan_locates_tracks_and_round_trips_through_framed_index_bytes() {
        let root = testdata_dir();
        assert!(
            root.is_dir(),
            "expected testdata fixtures at {}",
            root.display()
        );

        let roots = vec![root.clone()];
        let idx = CoreIndex::scan(roots.clone()).expect("scan should succeed on testdata/");

        assert_ne!(
            idx.version().0,
            0,
            "a fresh index must report a non-zero version"
        );

        let located = idx.locate(0).expect("track 0 should resolve to a path");
        assert!(
            located.starts_with(&root),
            "{} should live under {}",
            located.display(),
            root.display()
        );

        // Reference: scanning the same root directly through the backend.
        let direct = Library::scan_many(&roots).expect("direct scan_many for comparison");
        assert_eq!(direct.len(), idx.library.len());

        // Round-trip: frame -> deframe -> rebuild from bytes -> compare.
        let framed = idx.index_bytes().expect("index_bytes should succeed");
        assert!(
            !framed.is_empty(),
            "a non-empty library bakes non-empty bytes"
        );

        let blobs = deframe_index(&framed).expect("deframe_index should parse what we framed");
        assert_eq!(blobs.len(), roots.len(), "one blob per root");

        let rebuilt = Library::from_source_bytes(&roots, blobs);
        assert_eq!(rebuilt.len(), direct.len(), "len matches the direct scan");
        assert_eq!(
            rebuilt.source_counts(),
            direct.source_counts(),
            "source_counts match the direct scan"
        );
    }
}
