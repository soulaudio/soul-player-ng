pub trait IndexPort: Send + Sync {
    fn version(&self) -> super::IndexVersion;
    fn index_bytes(&self) -> anyhow::Result<Vec<u8>>;
    fn locate(&self, track_id: u32) -> Option<std::path::PathBuf>;
}

pub trait MediaStore: Send + Sync {
    fn open(&self, path: &std::path::Path) -> std::io::Result<Box<dyn ReadSeek>>;
    fn len(&self, path: &std::path::Path) -> std::io::Result<u64>;
}

pub trait ReadSeek: std::io::Read + std::io::Seek + Send {}

impl<T: std::io::Read + std::io::Seek + Send> ReadSeek for T {}

pub trait UserStore: Send + Sync {
    fn verify(&self, user: &str, pass: &str) -> Option<super::UserId>;
    fn add_user(&self, user: &str, pass: &str) -> anyhow::Result<super::UserId>;
}
