//! `FsMedia`: the [`MediaStore`](crate::domain::ports::MediaStore) adapter over
//! the local filesystem — opens track files directly by the path the
//! [`IndexPort`](crate::domain::ports::IndexPort) resolves them to.

use std::fs::File;
use std::io::{BufReader, Result};
use std::path::Path;

use crate::domain::ports::{MediaStore, ReadSeek};

/// [`MediaStore`] that reads tracks straight off the local filesystem.
pub struct FsMedia;

impl MediaStore for FsMedia {
    fn open(&self, path: &Path) -> Result<Box<dyn ReadSeek>> {
        Ok(Box::new(BufReader::new(File::open(path)?)))
    }

    fn len(&self, path: &Path) -> Result<u64> {
        Ok(std::fs::metadata(path)?.len())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn opens_and_reads_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("track.bin");
        std::fs::write(&path, b"some audio bytes").unwrap();

        let media = FsMedia;
        let mut reader = media.open(&path).unwrap();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).unwrap();

        assert_eq!(buf, b"some audio bytes");
        assert_eq!(media.len(&path).unwrap(), buf.len() as u64);
    }

    #[test]
    fn open_of_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.bin");

        let media = FsMedia;
        assert!(media.open(&missing).is_err());
        assert!(media.len(&missing).is_err());
    }
}
