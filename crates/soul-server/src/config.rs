//! TOML config file for the remote source server.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Server configuration loaded from a TOML file.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub bind: String,
    pub roots: Vec<PathBuf>,
    pub db: PathBuf,
}

/// Reads and parses the TOML config file at `path`.
pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("parsing config file {}", path.display()))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::load;
    use std::path::PathBuf;

    #[test]
    fn loads_bind_roots_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("soul-server.toml");
        std::fs::write(
            &path,
            r#"
bind = "127.0.0.1:0"
roots = ["/music"]
db = "u.db"
"#,
        )
        .unwrap();

        let config = load(&path).unwrap();

        assert_eq!(config.bind, "127.0.0.1:0");
        assert_eq!(config.roots, vec![PathBuf::from("/music")]);
        assert_eq!(config.db, PathBuf::from("u.db"));
    }
}
