//! Test-only fixture writers for app e2e tests: a minimal real FLAC carrying a
//! VORBIS_COMMENT (mirrors `soul-player-backend`'s private `flac_with_comments`,
//! which every backend scan test uses) and a `put` that writes a file at a
//! `/`-separated relative path, creating its folders.

use std::path::{Path, PathBuf};

/// A FLAC with STREAMINFO (44.1 kHz, stereo, 16-bit, 1 s) and the given
/// `KEY=value` comments, no audio frames: enough for the import + organize tag paths.
pub(crate) fn flac_with_comments(comments: &[&str]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"fLaC");
    v.push(0x00); // STREAMINFO (type 0), not last
    v.extend_from_slice(&[0, 0, 34]);
    let mut si = [0u8; 34];
    let (sr, ch, bps, total): (u32, u32, u32, u64) = (44_100, 2, 16, 44_100);
    si[10] = (sr >> 12) as u8;
    si[11] = (sr >> 4) as u8;
    let (sr_lo, ch_bits) = (((sr & 0xF) as u8) << 4, (((ch - 1) & 0x7) as u8) << 1);
    si[12] = sr_lo | ch_bits | (((bps - 1) >> 4) as u8);
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

/// Write `bytes` at `root/rel` (`rel` is `/`-separated), creating folders.
pub(crate) fn put(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in rel.split('/') {
        p.push(c);
    }
    std::fs::create_dir_all(p.parent().expect("rel has a parent")).unwrap();
    std::fs::write(&p, bytes).unwrap();
    p
}
