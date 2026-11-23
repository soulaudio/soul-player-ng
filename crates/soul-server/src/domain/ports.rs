/// The catalog the server serves: its current version and the baked index bytes, plus
/// resolving a track id to the file to stream.
pub trait IndexPort: Send + Sync {
    /// The catalog's current [`IndexVersion`](super::IndexVersion).
    fn version(&self) -> super::IndexVersion;
    /// The baked `.soulidx` bytes a client fetches to build its local catalog.
    fn index_bytes(&self) -> anyhow::Result<Vec<u8>>;
    /// Resolve a track id to the local file path to stream, or `None` if unknown.
    fn locate(&self, track_id: u32) -> Option<std::path::PathBuf>;
}

/// Byte access to the library's media files (the source for range streaming).
pub trait MediaStore: Send + Sync {
    /// Open a track file for seekable reading.
    fn open(&self, path: &std::path::Path) -> std::io::Result<Box<dyn ReadSeek>>;
    /// The track file's total length in bytes.
    fn len(&self, path: &std::path::Path) -> std::io::Result<u64>;
}

/// A readable + seekable + `Send` byte source (the object-safe handle [`MediaStore`] returns).
pub trait ReadSeek: std::io::Read + std::io::Seek + Send {}

impl<T: std::io::Read + std::io::Seek + Send> ReadSeek for T {}

/// The user/credential store backing login and user provisioning.
pub trait UserStore: Send + Sync {
    /// Verify a username/password, returning the user id on success.
    fn verify(&self, user: &str, pass: &str) -> Option<super::UserId>;
    /// Add a user with the given credentials, returning the new user id.
    fn add_user(&self, user: &str, pass: &str) -> anyhow::Result<super::UserId>;
}
