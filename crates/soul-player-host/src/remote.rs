//! `remote`: client side of the soul-server remote-source protocol —
//! authenticate against a server, fetch its catalog index, and download
//! track originals into a local cache.
//!
//! The server lives in the separate `soul-server` crate; this module speaks
//! its `/v0` wire protocol over plain `reqwest::blocking` HTTP without
//! depending on that crate at runtime (`soul-server` is a dev-dependency
//! here, only to drive this module's tests against a REAL in-process
//! server) — the wire format is the contract, not the crate.
//!
//! Defines [`TrackLocation`] and [`MediaFetch`] — the seam the (forthcoming)
//! loader consumes to resolve a catalog track to a local, playable path
//! regardless of whether it lives on disk or on a remote source.
//!
//! Nothing outside `#[cfg(test)]` wires this module's public API into the
//! rest of the crate yet — that's the loader task. Until then, `cargo
//! build`'s plain (non-test) pass sees every item here as unreachable from
//! the crate root and would otherwise warn on all of them.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};

/// A soul-server the player has authenticated against: its base URL (no
/// trailing slash) and the bearer token obtained at add-time via
/// [`authenticate`].
#[derive(Clone, Debug)]
pub struct RemoteServer {
    pub url: String,
    pub token: String,
}

/// Where a track's original media lives.
///
/// `Remote`'s `server` is an index into whatever list of [`RemoteServer`]s
/// the caller keeps (mirroring how [`soul_player_backend::Library`] already
/// addresses a track by a source index plus a root-relative path); `rel` is
/// that same root-relative path, and `track_id` is the id the remote
/// server's own catalog knows the track by (needed to GET `/v0/track/{id}`,
/// which is unrelated to any local id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackLocation {
    Local(PathBuf),
    Remote {
        server: usize,
        rel: PathBuf,
        track_id: u32,
    },
}

/// Resolves a remote track to a local, playable file path — downloading it
/// first if necessary. A trait so the loader can depend on the behavior
/// (and swap in a test fake) without caring that [`DownloadCache`] is the
/// only real implementation.
pub trait MediaFetch: Send + Sync {
    fn fetch(&self, server: &RemoteServer, track_id: u32) -> anyhow::Result<PathBuf>;
}

/// [`MediaFetch`] that downloads a track's original bytes into one flat
/// directory, keyed by catalog id, and skips the download outright on a
/// cache hit (a file already there, non-empty).
pub struct DownloadCache {
    pub dir: PathBuf,
}

impl DownloadCache {
    /// The cache file path for `track_id` — stable and extension-less, since
    /// the loader opens tracks by content/format sniffing rather than by
    /// file extension.
    fn cache_path(&self, track_id: u32) -> PathBuf {
        self.dir.join(track_id.to_string())
    }
}

impl MediaFetch for DownloadCache {
    fn fetch(&self, server: &RemoteServer, track_id: u32) -> anyhow::Result<PathBuf> {
        let path = self.cache_path(track_id);
        if fs::metadata(&path).is_ok_and(|meta| meta.len() > 0) {
            return Ok(path);
        }

        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating download cache dir {}", self.dir.display()))?;

        let endpoint = format!("{}/v0/track/{track_id}", server.url);
        let response = reqwest::blocking::Client::new()
            .get(&endpoint)
            .bearer_auth(&server.token)
            .send()
            .with_context(|| format!("GET {endpoint}"))?;
        if !response.status().is_success() {
            bail!("GET {endpoint} failed: {}", response.status());
        }
        let bytes = response
            .bytes()
            .with_context(|| format!("reading response body from {endpoint}"))?;

        // Download to a sibling temp file and rename into place, so a
        // crash mid-download can never leave a short/corrupt file sitting
        // at `path` masquerading as a cache hit.
        let tmp = self.dir.join(format!("{track_id}.part"));
        {
            let mut file =
                fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
            file.write_all(&bytes)
                .with_context(|| format!("writing {}", tmp.display()))?;
        }
        fs::rename(&tmp, &path)
            .with_context(|| format!("finalizing cache file {}", path.display()))?;

        Ok(path)
    }
}

/// Authenticate against `{url}/v0/auth` with a username/password and return
/// the issued bearer token. A non-2xx response (wrong credentials, server
/// error) is an `Err`.
pub fn authenticate(url: &str, user: &str, pass: &str) -> anyhow::Result<String> {
    let endpoint = format!("{url}/v0/auth");
    let response = reqwest::blocking::Client::new()
        .post(&endpoint)
        .json(&serde_json::json!({ "user": user, "pass": pass }))
        .send()
        .with_context(|| format!("POST {endpoint}"))?;
    if !response.status().is_success() {
        bail!("POST {endpoint} failed: {}", response.status());
    }

    let body: serde_json::Value = response
        .json()
        .with_context(|| format!("parsing auth response from {endpoint}"))?;
    body["token"]
        .as_str()
        .map(str::to_string)
        .context("auth response had no string `token` field")
}

/// Fetch `{server.url}/v0/index` (Bearer-authenticated) and split the
/// response's framed `.soulidx` bytes back into one blob per source root
/// (see [`deframe`]). A non-2xx response is an `Err`.
pub fn fetch_index(server: &RemoteServer) -> anyhow::Result<Vec<Vec<u8>>> {
    let endpoint = format!("{}/v0/index", server.url);
    let response = reqwest::blocking::Client::new()
        .get(&endpoint)
        .bearer_auth(&server.token)
        .send()
        .with_context(|| format!("GET {endpoint}"))?;
    if !response.status().is_success() {
        bail!("GET {endpoint} failed: {}", response.status());
    }

    let bytes = response
        .bytes()
        .with_context(|| format!("reading response body from {endpoint}"))?;
    deframe(&bytes)
}

/// Split a bundle framed as `[u32 LE n][per blob: u32 LE length, that many
/// bytes]` back into its `n` blobs.
///
/// This mirrors the server's `soul_server::index::frame_index`/`deframe_index`
/// byte-for-byte, reimplemented here rather than taken as a runtime
/// dependency on that crate — the wire format is the contract.
fn deframe(bytes: &[u8]) -> anyhow::Result<Vec<Vec<u8>>> {
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
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::mpsc;

    use soul_server::domain::ports::{IndexPort, UserStore};
    use soul_server::domain::service::Services;
    use soul_server::index::CoreIndex;
    use soul_server::media::FsMedia;
    use soul_server::proto::native::{self, AppServices};
    use soul_server::store::SqliteUsers;

    use super::*;

    /// Path to the repo's committed `testdata/` fixtures, three levels up
    /// from this crate's manifest dir to the repo root.
    fn testdata_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
    }

    /// A real `soul-server` native router, bound to an ephemeral localhost
    /// port and seeded with one user (`"u"`/`"p"`) over `testdata/`.
    struct TestServer {
        url: String,
        services: Arc<AppServices>,
        // Kept alive so the server's worker threads (and listening socket)
        // keep running for the test's duration; never read directly.
        _runtime: tokio::runtime::Runtime,
        _dir: tempfile::TempDir,
    }

    fn start_server() -> TestServer {
        let dir = tempfile::tempdir().expect("create temp dir");
        let index = CoreIndex::scan(vec![testdata_dir()]).expect("scan testdata/");
        let users = SqliteUsers::open(&dir.path().join("users.db")).expect("open sqlite store");
        users.add_user("u", "p").expect("seed test user");
        let services = Arc::new(Services {
            index,
            media: FsMedia,
            users,
        });

        let runtime = tokio::runtime::Runtime::new().expect("build tokio runtime");
        let (addr_tx, addr_rx) = mpsc::channel();
        let router_services = services.clone();
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind an ephemeral port");
            addr_tx
                .send(listener.local_addr().expect("bound socket has an addr"))
                .expect("send bound addr to the test thread");
            axum::serve(listener, native::router(router_services))
                .await
                .expect("serve the native router");
        });
        let addr = addr_rx
            .recv()
            .expect("server thread reports its bound address");

        TestServer {
            url: format!("http://{addr}"),
            services,
            _runtime: runtime,
            _dir: dir,
        }
    }

    #[test]
    fn authenticate_accepts_right_password_and_rejects_wrong_password() {
        let server = start_server();

        let token = authenticate(&server.url, "u", "p").expect("authenticate with right creds");
        assert!(!token.is_empty(), "expected a non-empty token");

        let err = authenticate(&server.url, "u", "wrong").expect_err("wrong password should error");
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn fetch_index_returns_blobs_that_rebuild_a_nonempty_library() {
        let server = start_server();
        let token = authenticate(&server.url, "u", "p").expect("authenticate");
        let remote = RemoteServer {
            url: server.url.clone(),
            token,
        };

        let blobs = fetch_index(&remote).expect("fetch_index");
        assert!(!blobs.is_empty(), "expected at least one per-root blob");
        for blob in &blobs {
            assert!(!blob.is_empty(), "each per-root blob should be non-empty");
        }

        let library =
            soul_player_backend::Library::from_source_bytes(&[PathBuf::from("remote")], blobs);
        assert!(!library.is_empty(), "rebuilt library should be non-empty");
    }

    #[test]
    fn fetch_index_rejects_a_bad_token() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            token: "not-a-real-token".to_string(),
        };

        assert!(fetch_index(&remote).is_err());
    }

    #[test]
    fn download_cache_writes_matching_bytes_and_hits_cache_on_the_second_fetch() {
        let server = start_server();
        let token = authenticate(&server.url, "u", "p").expect("authenticate");
        let remote = RemoteServer {
            url: server.url.clone(),
            token,
        };

        let expected_path = server
            .services
            .index
            .locate(0)
            .expect("track 0 resolves to a path");
        let expected_bytes = fs::read(&expected_path).expect("read the reference file");

        let cache_dir = tempfile::tempdir().expect("create cache dir");
        let cache = DownloadCache {
            dir: cache_dir.path().to_path_buf(),
        };

        let first = cache.fetch(&remote, 0).expect("first fetch downloads");
        assert_eq!(fs::read(&first).unwrap(), expected_bytes);

        // Mutate the cached file in a way a redownload would overwrite: if
        // the second `fetch` hit the network, it would restore
        // `expected_bytes`; if it's a cache hit, the mutation survives.
        let marker = b"not the real track bytes, just proving no redownload".to_vec();
        fs::write(&first, &marker).expect("mutate the cached file");

        let second = cache
            .fetch(&remote, 0)
            .expect("second fetch should hit cache, not redownload");
        assert_eq!(second, first, "cache hit should return the same path");
        assert_eq!(
            fs::read(&second).unwrap(),
            marker,
            "a cache hit must not re-fetch and overwrite the cached file"
        );
    }

    #[test]
    fn download_cache_fetch_of_unknown_track_errors() {
        let server = start_server();
        let token = authenticate(&server.url, "u", "p").expect("authenticate");
        let remote = RemoteServer {
            url: server.url.clone(),
            token,
        };

        let cache_dir = tempfile::tempdir().expect("create cache dir");
        let cache = DownloadCache {
            dir: cache_dir.path().to_path_buf(),
        };

        assert!(cache.fetch(&remote, 999_999).is_err());
    }

    #[test]
    fn deframe_round_trips_with_the_servers_frame_index() {
        let blobs = vec![b"hello".to_vec(), Vec::new(), b"a bit longer blob".to_vec()];
        let framed = soul_server::index::frame_index(&blobs);

        let back = deframe(&framed).expect("deframe should parse what the server framed");
        assert_eq!(back, blobs);
    }

    #[test]
    fn deframe_rejects_truncated_bytes() {
        let truncated = 1u32.to_le_bytes().to_vec();
        assert!(deframe(&truncated).is_err());
    }
}
