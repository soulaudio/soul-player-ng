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
//! Defines [`TrackLocation`] and [`MediaFetch`] — the seam the loader
//! ([`RemoteLibrary`], consumed by `PlayerHandle::add_remote_source`)
//! resolves a catalog track through to a local, playable path regardless of
//! whether it lives on disk or on a remote source.
//!
//! `TrackLocation` itself stays unconsumed for now (`RemoteLibrary::path_of`
//! goes straight from a compound id to a path without materializing one) —
//! reserved for a future caller that wants to know *why* a path resolved the
//! way it did (e.g. a "from <server>" badge); see the narrow `dead_code`
//! allow on it below instead of the old blanket one.
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, bail};
use soul_audio_multicatalog::{pack, unpack};
use soul_player_backend::library::{Library, TrackSource};

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
#[allow(
    dead_code,
    reason = "reserved for a future caller that wants to know *why* a path \
              resolved the way it did; RemoteLibrary::path_of resolves \
              straight to a PathBuf without materializing one"
)]
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

/// Per aggregated source (by position in the combined [`Library`] built by
/// [`RemoteLibrary::build`]): how to turn one of that source's track ids into
/// a local file.
enum SourceResolve {
    /// A local root: delegate straight to [`Library::path_of`].
    Local,
    /// One of `server`'s own catalog roots. `base` is the aggregate source
    /// index of this server's FIRST source — shared by every source in its
    /// block — needed to recover the server's own id (see
    /// [`RemoteLibrary::path_of`]).
    Remote {
        server: Arc<RemoteServer>,
        base: usize,
    },
}

/// A [`TrackSource`] over an aggregated [`Library`] whose sources are a mix
/// of local roots and remote servers: local ids resolve straight through the
/// aggregate; remote ids are downloaded (and cached) via `fetch`.
///
/// # The id mapping
///
/// [`Library::from_source_bytes`] (via the shared `MultiCatalog`) packs every
/// id as `(source, local)`: `source` is the position of that track's
/// root/blob in the combined list passed to `from_source_bytes` (the high
/// bits), `local` is the id within that one blob's own baked index (the low
/// bits) — see [`pack`]/[`unpack`]. A remote server bakes its OWN roots into
/// a `Library` the exact same way (`soul-server`'s index just wraps
/// `Library::scan_many_incremental`), so for the `j`-th root/blob of a server
/// occupying this aggregate's sources `[base, base + k)`, the server's own id
/// for a track is `pack(j, local)`, where `(source, local) = unpack(aggregate_id)`
/// and `j = source - base`.
///
/// Recovering `j` this way — through the packed `source` field, NOT a flat
/// `aggregate_id - base` subtraction — is the part that must stay correct: a
/// bare subtraction would mix the high-bits source index into the arithmetic
/// instead of isolating `local`, and silently fetch the wrong track (or
/// underflow) the moment any source precedes the remote block.
pub struct RemoteLibrary {
    lib: Arc<Library>,
    resolve: Vec<SourceResolve>,
    fetch: Arc<dyn MediaFetch>,
}

impl RemoteLibrary {
    /// Build the aggregate from `local` (root, baked-bytes) pairs followed by
    /// each remote server's own (server, per-root blobs) in order — sources
    /// are laid out local-first, then one block per remote server, in the
    /// order given. Each remote blob gets a sentinel root path (never read as
    /// a real filesystem path; it only occupies `Library`'s per-source root
    /// list so `source_counts` / compound ids line up with `resolve`).
    #[must_use]
    pub fn build(
        local: Vec<(PathBuf, Vec<u8>)>,
        remotes: Vec<(Arc<RemoteServer>, Vec<Vec<u8>>)>,
        fetch: Arc<dyn MediaFetch>,
    ) -> Self {
        let mut roots = Vec::new();
        let mut blobs = Vec::new();
        let mut resolve = Vec::new();

        for (root, bytes) in local {
            roots.push(root);
            blobs.push(bytes);
            resolve.push(SourceResolve::Local);
        }

        for (server, server_blobs) in remotes {
            // Shared by every source this server contributes below.
            let base = resolve.len();
            for (i, blob) in server_blobs.into_iter().enumerate() {
                roots.push(PathBuf::from(format!("remote:{}#{i}", server.url)));
                blobs.push(blob);
                resolve.push(SourceResolve::Remote {
                    server: Arc::clone(&server),
                    base,
                });
            }
        }

        let lib = Arc::new(Library::from_source_bytes(&roots, blobs));
        Self {
            lib,
            resolve,
            fetch,
        }
    }

    /// The aggregate library, for browsing/views — `path_of`
    /// ([`TrackSource`]) is what resolves a track to a playable file,
    /// downloading a remote one first.
    #[must_use]
    pub fn library(&self) -> Arc<Library> {
        Arc::clone(&self.lib)
    }
}

impl TrackSource for RemoteLibrary {
    fn path_of(&self, id: u32) -> Option<PathBuf> {
        let located = self.lib.locate(id)?;
        match self.resolve.get(located.source)? {
            SourceResolve::Local => self.lib.path_of(id),
            SourceResolve::Remote { server, base } => {
                let (_, local) = unpack(id);
                let j = u32::try_from(located.source.checked_sub(*base)?).ok()?;
                self.fetch.fetch(server, pack(j, local)).ok()
            }
        }
    }

    fn duration_of(&self, id: u32) -> Option<f64> {
        self.lib.duration_of(id)
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
        start_server_over(vec![testdata_dir()])
    }

    /// Like [`start_server`], but scanning `roots` instead of the whole
    /// `testdata/` tree — so a test can stand up a server over a DIFFERENT
    /// subset of fixtures than whatever it scanned locally.
    fn start_server_over(roots: Vec<PathBuf>) -> TestServer {
        let dir = tempfile::tempdir().expect("create temp dir");
        let index = CoreIndex::scan(roots).expect("scan the given roots");
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

    /// Bake `root` (a single-root scan) and hand back its one `.soulidx`
    /// blob — reuses the server's own `CoreIndex`/framing so this is exactly
    /// the shape `RemoteLibrary::build`'s `local` and `fetch_index`'s remote
    /// blobs both take.
    fn scan_blob(root: &std::path::Path) -> Vec<u8> {
        let index = CoreIndex::scan(vec![root.to_path_buf()]).expect("scan the given root");
        let framed = index.index_bytes().expect("frame the scanned index");
        let mut blobs = deframe(&framed).expect("deframe what we just framed");
        assert_eq!(blobs.len(), 1, "a single-root scan bakes one blob");
        blobs.remove(0)
    }

    #[test]
    fn remote_library_resolves_local_and_remote_tracks_with_a_nonzero_base() {
        // A local source with exactly ONE track, so its single id is
        // unambiguous, followed (in the aggregate) by a remote block — this
        // gives the remote server's sources a nonzero `base`, which is
        // exactly what would expose a bare `id - base` subtraction instead
        // of the correct `(source, local)` unpack/repack.
        let local_dir = tempfile::tempdir().expect("local subset dir");
        fs::copy(
            testdata_dir().join("meta.flac"),
            local_dir.path().join("meta.flac"),
        )
        .expect("copy the local fixture");
        let local_root = local_dir.path().to_path_buf();
        let local_blob = scan_blob(&local_root);
        let local_count =
            Library::from_source_bytes(std::slice::from_ref(&local_root), vec![local_blob.clone()])
                .len();
        assert_eq!(local_count, 1, "expected exactly one local track");

        // A DIFFERENT subset of fixtures, served by a REAL in-process server
        // the local scan above never touched.
        let remote_src_dir = tempfile::tempdir().expect("remote subset dir");
        for name in ["tone.flac", "tagged.flac"] {
            fs::copy(testdata_dir().join(name), remote_src_dir.path().join(name))
                .unwrap_or_else(|e| panic!("copy remote fixture {name}: {e}"));
        }
        let server = start_server_over(vec![remote_src_dir.path().to_path_buf()]);
        let token = authenticate(&server.url, "u", "p").expect("authenticate");
        let remote_server = Arc::new(RemoteServer {
            url: server.url.clone(),
            token,
        });
        let remote_blobs = fetch_index(&remote_server).expect("fetch_index");
        let remote_count = Library::from_source_bytes(
            &vec![PathBuf::from("probe"); remote_blobs.len()],
            remote_blobs.clone(),
        )
        .len();
        assert_eq!(remote_count, 2, "expected both remote fixtures scanned");

        let cache_dir = tempfile::tempdir().expect("download cache dir");
        let fetch: Arc<dyn MediaFetch> = Arc::new(DownloadCache {
            dir: cache_dir.path().to_path_buf(),
        });
        let remote_lib = RemoteLibrary::build(
            vec![(local_root.clone(), local_blob)],
            vec![(Arc::clone(&remote_server), remote_blobs)],
            fetch,
        );

        let agg = remote_lib.library();
        assert_eq!(
            agg.len(),
            local_count + remote_count,
            "aggregate should hold every local and remote track"
        );

        // The local id (source 0, the aggregate's only local source) resolves
        // to the real local file.
        let local_id = pack(0, 0);
        let local_path = remote_lib
            .path_of(local_id)
            .expect("local id should resolve to a path");
        assert_eq!(
            fs::read(&local_path).expect("read resolved local path"),
            fs::read(testdata_dir().join("meta.flac")).expect("read the original fixture"),
            "a local id should resolve straight to the real local file"
        );

        // Remote ids live in source 1 (base = 1, the single local source
        // before them) — a nonzero base. For each of the server's own track
        // ids, the aggregate id is `pack(1, server_id)`; resolving it must
        // download bytes identical to what the server itself would hand back
        // for that SAME server-side id, proving the (source, local) unpack
        // and repack — not a flat subtraction — recovered it correctly.
        for server_id in 0..remote_count as u32 {
            let expected_path = server
                .services
                .index
                .locate(server_id)
                .unwrap_or_else(|| panic!("server should locate its own id {server_id}"));
            let expected_bytes =
                fs::read(&expected_path).expect("read the server's reference file");

            let aggregate_id = pack(1, server_id);
            let downloaded = remote_lib.path_of(aggregate_id).unwrap_or_else(|| {
                panic!("remote id {aggregate_id} (server id {server_id}) should resolve")
            });
            assert_eq!(
                fs::read(&downloaded).expect("read the downloaded cache file"),
                expected_bytes,
                "server id {server_id} (aggregate id {aggregate_id}) should download \
                 byte-identical to the server's own file"
            );
        }
    }
}
