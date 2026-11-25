//! TOML config file for the remote source server.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Server configuration loaded from a TOML file.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// The socket address to bind the HTTP server to (e.g. `0.0.0.0:8080`).
    pub bind: String,
    /// The library root directories the server indexes and serves.
    pub roots: Vec<PathBuf>,
    /// Path to the server's database file (users / credentials).
    pub db: PathBuf,
    /// The base URL clients reach this server on, when that differs from
    /// `bind`, i.e. whenever a reverse proxy terminates TLS in front of it
    /// (`https://music.example.com`). Every OAuth endpoint the discovery
    /// document advertises is built from this, so a proxied server that leaves
    /// it unset hands clients its unreachable internal address. Unset means
    /// clients reach `bind` directly.
    #[serde(default)]
    pub public_url: Option<String>,
}

impl Config {
    /// The server's issuer URL: `public_url` when set, else `http://{bind}`.
    ///
    /// Any trailing slash is trimmed: endpoints are built as `{issuer}/token`,
    /// and `https://host//token` is a different URL to most proxies.
    ///
    /// Changing this invalidates tokens already issued: the `iss` claim is
    /// stamped at mint time and checked on every request.
    pub fn issuer(&self) -> String {
        match &self.public_url {
            Some(url) => url.trim_end_matches('/').to_string(),
            None => format!("http://{}", self.bind),
        }
    }
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
        assert_eq!(config.public_url, None);
    }

    /// Without a proxy the issuer is the bound address, what a client that
    /// connects straight to `bind` resolves discovery against.
    #[test]
    fn issuer_falls_back_to_the_bind_address() {
        let config = parse(
            r#"
bind = "0.0.0.0:8080"
roots = ["/music"]
db = "u.db"
"#,
        );

        assert_eq!(config.issuer(), "http://0.0.0.0:8080");
    }

    /// Behind a TLS proxy the discovery document must advertise the public
    /// origin; the internal bind address is unreachable from the client.
    #[test]
    fn public_url_overrides_the_bind_address_and_loses_its_trailing_slash() {
        let config = parse(
            r#"
bind = "127.0.0.1:8080"
roots = ["/music"]
db = "u.db"
public_url = "https://music.example.com/"
"#,
        );

        assert_eq!(config.issuer(), "https://music.example.com");
    }

    fn parse(text: &str) -> super::Config {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("soul-server.toml");
        std::fs::write(&path, text).unwrap();
        load(&path).unwrap()
    }
}
