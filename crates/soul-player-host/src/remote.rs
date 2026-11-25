//! `remote`: client side of the soul-server remote-source protocol,
//! authenticate against a server, fetch its catalog index, and download
//! track originals into a local cache.
//!
//! The server lives in the separate `soul-server` crate; this module speaks
//! its `/v0` wire protocol over plain `reqwest::blocking` HTTP without
//! depending on that crate at runtime (`soul-server` is a dev-dependency
//! here, only to drive this module's tests against a REAL in-process
//! server), the wire format is the contract, not the crate.
//!
//! Defines [`TrackLocation`] and [`MediaFetch`], the seam the loader
//! ([`RemoteLibrary`], consumed by `PlayerHandle::add_remote_source`)
//! resolves a catalog track through to a local, playable path regardless of
//! whether it lives on disk or on a remote source.
//!
//! `TrackLocation` itself stays unconsumed for now (`RemoteLibrary::path_of`
//! goes straight from a compound id to a path without materializing one),
//! reserved for a future caller that wants to know *why* a path resolved the
//! way it did (e.g. a "from <server>" badge); see the narrow `dead_code`
//! allow on it below instead of the old blanket one.
//!
//! [`HttpRangeReader`] is the streaming alternative to [`MediaFetch`]'s
//! download-then-play: a `Read + Seek` over `GET /v0/track/{id}` HTTP Range
//! requests, opened directly into a decoder with no materialized file. It's
//! wired up as [`RemoteLibrary`]'s [`TrackSource::open_decoder`] override,
//! the loader's entry point for actually playing a track: a remote id streams
//! through this reader by default, falling back to [`MediaFetch`]'s
//! download-then-play only if the stream can't be opened.
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use soul_audio_desktop::{OwnedDecoder, open_seekable_src, open_track};
use soul_audio_multicatalog::{pack, unpack};
use soul_player_backend::library::{IndexBlob, Library, TrackSource};

use crate::RemoteHealth;

/// A soul-server the player has logged into via OAuth: its base URL (no
/// trailing slash) and the short-lived JWT access token obtained via
/// [`login`] (or [`exchange_code`]/[`refresh`]) and carried OPAQUELY, this
/// client never parses or validates it, it only ever forwards it as a
/// bearer token.
#[derive(Clone, Debug)]
pub struct RemoteServer {
    /// The server's base URL (e.g. `https://listen.home.lan:8080`).
    pub url: String,
    /// The OAuth bearer token, carried opaquely, this client never parses it.
    pub access_token: String,
}

/// `url`'s host (and port, if non-default) for display, e.g. as a per-track
/// "streams from" tooltip, a bare `https://` isn't informative, and the
/// whole URL carries path noise a label doesn't need. Falls back to the
/// whole `url` if it carries no recognizable scheme.
///
/// Mirrors `app`'s `views::settings::display_host` (a remote-source row's
/// title line uses the exact same rule), duplicated here rather than shared:
/// the host crate and the Rune UI crate sit in separate cargo workspaces with
/// no dependency edge between them, so this one small helper is cheaper to
/// keep in sync by inspection than to split into a new shared crate for.
pub(crate) fn host_label(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

/// Connect timeout for every remote HTTP request: a server that does not accept
/// the connection within this window is treated as unreachable instead of hanging
/// the calling thread. `reqwest::blocking` has NO default timeout.
const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Total timeout for a BOUNDED remote request (the framed index, an OAuth token or
/// discovery document, or one 512 KiB range chunk): generous for the payload but
/// finite, so a server that stalls mid-response can't hang playback or a sync.
const HTTP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Total timeout for a WHOLE-track download ([`DownloadCache`] pulls the entire
/// file in one request): a large track on a slow link legitimately takes a while,
/// so this is generous, but still finite so a stalled server fails instead of
/// hanging the loader forever.
const HTTP_DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// A blocking HTTP client with a connect timeout plus the given total-request
/// timeout, so a dead or stalled remote server fails fast instead of hanging the
/// calling thread forever. `reqwest::blocking` sets neither timeout by default, so
/// every remote call in this module builds its client through here rather than via
/// `Client::new()`. Panics only where `Client::new()` already would (a TLS/backend
/// init failure), so this is a drop-in with the same contract plus the timeouts.
fn http_client(request_timeout: std::time::Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .build()
        .expect("a blocking HTTP client with static timeout config always builds")
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
    /// A track on the local filesystem at this path.
    Local(PathBuf),
    /// A track on a remote server.
    Remote {
        /// Index into the caller's list of [`RemoteServer`]s.
        server: usize,
        /// The track's root-relative path on that server.
        rel: PathBuf,
        /// The id the remote server's own catalog knows the track by (for `/v0/track/{id}`).
        track_id: u32,
    },
}

/// Resolves a remote track to a local, playable file path, downloading it
/// first if necessary. A trait so the loader can depend on the behavior
/// (and swap in a test fake) without caring that [`DownloadCache`] is the
/// only real implementation.
pub trait MediaFetch: Send + Sync {
    /// Resolve a remote track to a local, playable file path, downloading it first
    /// if it isn't already cached.
    fn fetch(&self, server: &RemoteServer, track_id: u32) -> anyhow::Result<PathBuf>;
}

/// [`MediaFetch`] that downloads a track's original bytes into one flat
/// directory, keyed by catalog id, and skips the download outright on a
/// cache hit (a file already there, non-empty).
pub struct DownloadCache {
    /// The flat directory downloaded track bytes are cached under.
    pub dir: PathBuf,
}

impl DownloadCache {
    /// This server's own cache subdirectory, named by a stable hash of its URL.
    /// Track ids are per-server (compound catalog ids), so two different servers
    /// routinely emit the same low id, keeping each server's downloads in its own
    /// namespace prevents serving one server's bytes for another's track.
    fn server_dir(&self, server: &RemoteServer) -> PathBuf {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        server.url.hash(&mut hasher);
        self.dir.join(format!("{:016x}", hasher.finish()))
    }
    /// The cache file path for `server`'s `track_id`, stable and extension-less,
    /// since the loader opens tracks by content/format sniffing rather than by
    /// file extension.
    fn cache_path(&self, server: &RemoteServer, track_id: u32) -> PathBuf {
        self.server_dir(server).join(track_id.to_string())
    }
}

impl MediaFetch for DownloadCache {
    fn fetch(&self, server: &RemoteServer, track_id: u32) -> anyhow::Result<PathBuf> {
        let dir = self.server_dir(server);
        let path = self.cache_path(server, track_id);
        if fs::metadata(&path).is_ok_and(|meta| meta.len() > 0) {
            return Ok(path);
        }

        fs::create_dir_all(&dir)
            .with_context(|| format!("creating download cache dir {}", dir.display()))?;

        let endpoint = format!("{}/v0/track/{track_id}", server.url);
        let response = http_client(HTTP_DOWNLOAD_TIMEOUT)
            .get(&endpoint)
            .bearer_auth(&server.access_token)
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
        let tmp = dir.join(format!("{track_id}.part"));
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
    /// index of this server's FIRST source, shared by every source in its
    /// block, needed to recover the server's own id (see
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
/// bits), see [`pack`]/[`unpack`]. A remote server bakes its OWN roots into
/// a `Library` the exact same way (`soul-server`'s index just wraps
/// `Library::scan_many_incremental`), so for the `j`-th root/blob of a server
/// occupying this aggregate's sources `[base, base + k)`, the server's own id
/// for a track is `pack(j, local)`, where `(source, local) = unpack(aggregate_id)`
/// and `j = source - base`.
///
/// Recovering `j` this way, through the packed `source` field, NOT a flat
/// `aggregate_id - base` subtraction, is the part that must stay correct: a
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
    /// each remote server's own (server, per-root blobs) in order, sources
    /// are laid out local-first, then one block per remote server, in the
    /// order given. Each remote blob gets a sentinel root path (never read as
    /// a real filesystem path; it only occupies `Library`'s per-source root
    /// list so `source_counts` / compound ids line up with `resolve`).
    #[must_use]
    pub fn build(
        local: Vec<(PathBuf, IndexBlob)>,
        remotes: Vec<(Arc<RemoteServer>, Vec<IndexBlob>)>,
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

    /// The aggregate library, for browsing/views, `path_of`
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

    /// A **local** id delegates straight to the local `Library`'s own
    /// `open_decoder` (the trait default, `path_of` then `open_track`,
    /// unchanged). A **remote** one STREAMS the track over HTTP Range
    /// ([`HttpRangeReader`] opened straight into [`open_seekable_src`])
    /// instead of downloading it first, so playback can start on the first
    /// fetched window rather than waiting for (or requiring) the whole file
    /// on disk. If the stream can't be opened, the server rejects Range, a
    /// transient network error, the track is otherwise unreachable that way,
    /// this falls back to the same download-to-cache path
    /// [`path_of`](Self::path_of) has always used, so a track that can't
    /// stream still plays.
    fn open_decoder(&self, id: u32) -> anyhow::Result<OwnedDecoder> {
        let located = self
            .lib
            .locate(id)
            .ok_or_else(|| anyhow::anyhow!("no such track id {id}"))?;
        let resolve = self
            .resolve
            .get(located.source)
            .ok_or_else(|| anyhow::anyhow!("no source resolver for track id {id}"))?;
        match resolve {
            SourceResolve::Local => self.lib.open_decoder(id),
            SourceResolve::Remote { server, base } => {
                let (_, local) = unpack(id);
                let j = u32::try_from(located.source.checked_sub(*base).ok_or_else(|| {
                    anyhow::anyhow!(
                        "track id {id}'s source {} precedes its remote block's base {base}",
                        located.source
                    )
                })?)
                .map_err(|_| {
                    anyhow::anyhow!("remote source index {} doesn't fit in u32", located.source)
                })?;
                let server_track_id = pack(j, local);

                let streamed =
                    HttpRangeReader::new(server, server_track_id).and_then(open_seekable_src);
                match streamed {
                    Ok(decoder) => Ok(decoder),
                    Err(_stream_err) => {
                        // Streaming didn't work out, fall back to the
                        // download-to-cache path the loader used before Inc 4b
                        // (also what RG-tag reading still goes through via
                        // `path_of`, so this is a well-trodden path, not a new
                        // failure mode).
                        let path = self.fetch.fetch(server, server_track_id)?;
                        open_track(&path)
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP-Range-backed streaming reader (Inc 4b): a `Read + Seek` over
// `GET /v0/track/{id}` for a decoder to open directly, as the streaming
// alternative to `DownloadCache`'s download-then-play. Self-contained: a
// SEPARATE later increment wires a decoder over this via core's
// `open_seekable`.
// ---------------------------------------------------------------------------

/// How many bytes to pull per HTTP Range request: large enough that a
/// decoder's small, frequent reads during sequential playback don't turn
/// into one request apiece, small enough to keep memory and first-byte
/// latency reasonable.
const CHUNK_SIZE: u64 = 512 * 1024;

/// The current in-memory window of fetched bytes: `[start, start +
/// bytes.len())` of the track, in absolute track-byte offsets.
struct Window {
    start: u64,
    bytes: Vec<u8>,
}

/// A read-ahead fetch of the NEXT sequential window, running on a background
/// thread. Started once a read crosses the current window's prefetch threshold,
/// so the read that eventually runs past the window joins an already-in-flight
/// (usually finished) fetch instead of paying a full round-trip inline on the
/// consuming thread (the audio pump during playback). `start` is the absolute
/// track offset this fetch is for, so a read can tell whether the pending
/// prefetch is the window it now needs or a stale one left behind by a seek.
struct Prefetch {
    start: u64,
    handle: JoinHandle<io::Result<(Vec<u8>, u64)>>,
}

/// A `std::io::Read + Seek` stream over a remote track's bytes, backed by
/// HTTP Range requests against `GET {url}/v0/track/{id}` rather than
/// downloading the whole file up front (see [`DownloadCache`] for that
/// alternative). Reads are served from a small in-memory [`Window`]; a read
/// that runs past it (or a [`Seek`] that lands outside it) triggers exactly
/// one more Range request, fetched lazily on the next `read`.
///
/// Built so a decoder can be opened directly over it on the loader thread,
/// `Send` (every field is), no interior mutability shared with anything
/// else, no `unsafe`.
pub struct HttpRangeReader {
    client: reqwest::blocking::Client,
    url: String,
    track_id: u32,
    access_token: String,
    /// The track's total byte length, learned from the first request's
    /// `Content-Range` header (see [`HttpRangeReader::new`]) and stable for
    /// the reader's whole lifetime.
    total_len: u64,
    /// The next byte offset [`Read::read`] will serve, and
    /// [`Seek::seek`]'s current position.
    pos: u64,
    /// `None` right after a [`Seek`] that landed outside the current
    /// window, cleared rather than refetched immediately, so a seek itself
    /// never touches the network; the next `read` refetches at `pos`.
    window: Option<Window>,
    /// An in-flight read-ahead of the next window, if one has been started for
    /// the current sequential run (see [`Prefetch`]). At most one runs at a
    /// time. Dropped when the reader drops: the worker owns its own HTTP client
    /// and finishes on its own (bounded by the request timeout), so no join is
    /// forced on the dropping thread (which may be the pump or loader).
    prefetch: Option<Prefetch>,
}

impl HttpRangeReader {
    /// Opens a streaming reader over `server`'s track `track_id`. Issues one
    /// HTTP Range request up front (`bytes=0-<CHUNK_SIZE-1>`) to learn the
    /// track's total length AND prime the window for the common case of
    /// reading sequentially from the start, rather than a separate `HEAD`-ish
    /// probe followed by a second request for the first chunk.
    pub fn new(server: &RemoteServer, track_id: u32) -> anyhow::Result<Self> {
        let mut reader = Self {
            client: http_client(HTTP_REQUEST_TIMEOUT),
            url: server.url.clone(),
            track_id,
            access_token: server.access_token.clone(),
            total_len: 0,
            pos: 0,
            window: None,
            prefetch: None,
        };
        let (bytes, total_len) = reader
            .fetch_chunk(0)
            .with_context(|| format!("opening HttpRangeReader for track {track_id}"))?;
        reader.total_len = total_len;
        reader.window = Some(Window { start: 0, bytes });
        Ok(reader)
    }

    /// The track's total byte length (learned at construction time).
    // Not a collection: there's no meaningful `is_empty` beyond `len() ==
    // 0`, which a caller can already spell directly.
    #[allow(clippy::len_without_is_empty)]
    #[must_use]
    pub fn len(&self) -> u64 {
        self.total_len
    }

    /// Whether `pos` falls within the currently buffered [`Window`].
    fn window_contains(&self, pos: u64) -> bool {
        self.window
            .as_ref()
            .is_some_and(|w| pos >= w.start && pos < w.start + w.bytes.len() as u64)
    }

    /// If a read-ahead for exactly `start` is in flight, take it and block on its
    /// result (often already ready); otherwise `None`, so the caller fetches
    /// synchronously. A pending prefetch for a DIFFERENT start, left behind by a
    /// seek, is discarded here (its handle dropped, detaching the worker) so it is
    /// never mistaken for the window now needed.
    fn take_prefetched(&mut self, start: u64) -> Option<io::Result<(Vec<u8>, u64)>> {
        match self.prefetch.take() {
            Some(p) if p.start == start => Some(match p.handle.join() {
                Ok(result) => result,
                Err(_) => Err(io::Error::other("remote read-ahead worker panicked")),
            }),
            _ => None,
        }
    }

    /// Start a background read-ahead of the next sequential window once the read
    /// position has passed the current window's halfway point, giving the fetch a
    /// head start before the window is exhausted. No-op if one is already in flight,
    /// if there is no window yet, or if the next window would begin at/after EOF.
    fn maybe_prefetch(&mut self) {
        if self.prefetch.is_some() {
            return;
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let window_len = window.bytes.len() as u64;
        let next_start = window.start + window_len;
        // Nothing past this window, or not enough of it consumed yet to justify
        // holding a second window resident.
        if next_start >= self.total_len || self.pos < window.start + window_len / 2 {
            return;
        }
        let client = self.client.clone();
        let url = self.url.clone();
        let access_token = self.access_token.clone();
        let track_id = self.track_id;
        let handle = std::thread::Builder::new()
            .name("remote-prefetch".into())
            .spawn(move || fetch_range(&client, &url, track_id, &access_token, next_start))
            .expect("spawn remote read-ahead worker");
        self.prefetch = Some(Prefetch {
            start: next_start,
            handle,
        });
    }

    /// Test hook: the absolute start offset of the in-flight read-ahead, if any.
    #[cfg(test)]
    fn prefetch_start(&self) -> Option<u64> {
        self.prefetch.as_ref().map(|p| p.start)
    }

    /// Fetches one [`CHUNK_SIZE`] window starting at `start` via an HTTP
    /// Range request, returning the fetched bytes alongside the track's
    /// total length (parsed from the response's `Content-Range` header).
    ///
    /// The server clamps an over-long range to end-of-file (see
    /// `soul_server::proto::native::track`), so this never errors just
    /// because `start + CHUNK_SIZE` runs past EOF, except when the WHOLE
    /// track is empty, in which case the server answers `416 Range Not
    /// Satisfiable` for any range at all; that degenerate case is reported
    /// back as `(no bytes, length 0)` rather than an `Err`, so opening a
    /// reader over an empty track still succeeds (immediately at EOF).
    fn fetch_chunk(&self, start: u64) -> io::Result<(Vec<u8>, u64)> {
        fetch_range(
            &self.client,
            &self.url,
            self.track_id,
            &self.access_token,
            start,
        )
    }
}

/// Fetch one [`CHUNK_SIZE`] window starting at `start` via an HTTP Range request,
/// returning the bytes plus the track's total length. A free function (not a method)
/// so the read-ahead worker thread can call it with owned clones of the connection
/// fields, without borrowing the `HttpRangeReader`. Same contract as
/// [`HttpRangeReader::fetch_chunk`]: a `416` on a wholly empty track is reported as
/// `(no bytes, length 0)`, not an error.
fn fetch_range(
    client: &reqwest::blocking::Client,
    url: &str,
    track_id: u32,
    access_token: &str,
    start: u64,
) -> io::Result<(Vec<u8>, u64)> {
    let end = start.saturating_add(CHUNK_SIZE - 1);
    let endpoint = format!("{url}/v0/track/{track_id}");
    let response = client
        .get(&endpoint)
        .bearer_auth(access_token)
        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
        .send()
        .map_err(io::Error::other)?;

    if response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        return Ok((Vec::new(), 0));
    }
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(io::Error::other(format!(
            "GET {endpoint} (Range: bytes={start}-{end}) returned {}",
            response.status()
        )));
    }

    let total_len = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_content_range_total)
        .ok_or_else(|| {
            io::Error::other("206 Partial Content response had no parsable Content-Range header")
        })?;

    let bytes = response.bytes().map_err(io::Error::other)?;
    Ok((bytes.to_vec(), total_len))
}

/// Parses the `TOTAL` out of a `Content-Range: bytes START-END/TOTAL` header
/// value.
fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit_once('/')?.1.parse().ok()
}

impl Read for HttpRangeReader {
    /// Serves bytes from the current window, growing/sliding it with one
    /// Range request whenever `pos` has moved outside it. Returns `Ok(0)` at
    /// EOF (`pos >= total_len`) per the trait's contract, rather than
    /// issuing a request the server would just reject.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.total_len {
            return Ok(0);
        }

        if !self.window_contains(self.pos) {
            // Prefer a read-ahead already fetching this exact window (started when
            // a prior read crossed the previous window's halfway point); fall back
            // to a synchronous fetch after a seek or at the very first read.
            let (bytes, _total_len) = match self.take_prefetched(self.pos) {
                Some(result) => result?,
                None => self.fetch_chunk(self.pos)?,
            };
            self.window = Some(Window {
                start: self.pos,
                bytes,
            });
        }

        let window = self
            .window
            .as_ref()
            .expect("window was just populated above");
        let offset = (self.pos - window.start) as usize;
        let available = &window.bytes[offset..];
        let n = available.len().min(buf.len());
        buf[..n].copy_from_slice(&available[..n]);
        self.pos += n as u64;
        // Line up the next window before this one runs out, so the read that
        // crosses the boundary joins a finished fetch instead of blocking on one
        // (the pump thread must never stall on a network round-trip mid-playback).
        self.maybe_prefetch();
        Ok(n)
    }
}

impl Seek for HttpRangeReader {
    /// Computes the new absolute position and, if it falls outside the
    /// current window, drops the window so the next [`Read::read`] refetches
    /// at the new `pos`, this method itself never touches the network
    /// (`total_len` is already known from construction, so even
    /// `SeekFrom::End` is a pure offset computation).
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(delta) => self.total_len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        let new_pos = new_pos.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to a negative or overflowing position",
            )
        })?;

        self.pos = new_pos;
        if !self.window_contains(self.pos) {
            self.window = None;
        }
        Ok(self.pos)
    }
}

/// Where the desktop client persists the OAuth **refresh token** obtained at
/// login, keyed by the remote server's base URL (so logging into several
/// servers keeps independent tokens). A trait so production code can depend
/// on [`KeyringStore`] (the OS keychain) while tests swap in [`MemStore`]
/// without touching the real Credential Manager/Keychain/Secret Service.
///
/// The OAuth **access** token is short-lived and never persisted here, only
/// kept in memory on [`RemoteServer`]; only the long-lived refresh token
/// needs durable, secret storage.
pub trait TokenStore: Send + Sync {
    /// Persist `refresh` for `server_url`, overwriting any previous value.
    fn save_refresh(&self, server_url: &str, refresh: &str) -> anyhow::Result<()>;
    /// Look up the refresh token for `server_url`, or `None` if there isn't one.
    fn load_refresh(&self, server_url: &str) -> Option<String>;
    /// Remove the refresh token for `server_url`. Deleting an entry that was
    /// never saved (or already deleted) is not an error.
    fn delete_refresh(&self, server_url: &str) -> anyhow::Result<()>;
}

/// [`TokenStore`] backed by the OS keychain (Windows Credential Manager,
/// macOS Keychain, Linux Secret Service) via the `keyring` crate. Every
/// server's refresh token is stored under the fixed service name
/// `"soul-player"` with the server's base URL as the account name, so
/// multiple servers never collide.
pub struct KeyringStore;

impl KeyringStore {
    const SERVICE: &'static str = "soul-player";
}

impl TokenStore for KeyringStore {
    fn save_refresh(&self, server_url: &str, refresh: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(Self::SERVICE, server_url)
            .with_context(|| format!("opening keyring entry for {server_url}"))?;
        entry
            .set_password(refresh)
            .with_context(|| format!("saving refresh token for {server_url}"))
    }

    fn load_refresh(&self, server_url: &str) -> Option<String> {
        let entry = keyring::Entry::new(Self::SERVICE, server_url).ok()?;
        match entry.get_password() {
            Ok(refresh) => Some(refresh),
            Err(keyring::Error::NoEntry) => None,
            Err(_) => None,
        }
    }

    fn delete_refresh(&self, server_url: &str) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(Self::SERVICE, server_url)
            .with_context(|| format!("opening keyring entry for {server_url}"))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            // Deleting an entry that was never saved (or already deleted)
            // is not an error, the caller just wants it gone.
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("deleting refresh token for {server_url}")),
        }
    }
}

/// In-memory [`TokenStore`] for tests: round-trips through a `Mutex<HashMap>`
/// keyed by server URL, never touching the real OS keychain.
#[derive(Default)]
pub struct MemStore(std::sync::Mutex<std::collections::HashMap<String, String>>);

impl MemStore {
    /// An empty in-memory token store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl TokenStore for MemStore {
    fn save_refresh(&self, server_url: &str, refresh: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .expect("MemStore mutex poisoned")
            .insert(server_url.to_string(), refresh.to_string());
        Ok(())
    }

    fn load_refresh(&self, server_url: &str) -> Option<String> {
        self.0
            .lock()
            .expect("MemStore mutex poisoned")
            .get(server_url)
            .cloned()
    }

    fn delete_refresh(&self, server_url: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .expect("MemStore mutex poisoned")
            .remove(server_url);
        Ok(())
    }
}

/// Fetch `{server.url}/v0/index` (Bearer-authenticated) and split the
/// response's framed `.soulidx` bytes back into one blob per source root
/// (see [`deframe`]). A non-2xx response is an `Err`.
pub fn fetch_index(server: &RemoteServer) -> anyhow::Result<Vec<IndexBlob>> {
    let endpoint = format!("{}/v0/index", server.url);
    let response = http_client(HTTP_REQUEST_TIMEOUT)
        .get(&endpoint)
        .bearer_auth(&server.access_token)
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
/// dependency on that crate, the wire format is the contract.
fn deframe(bytes: &[u8]) -> anyhow::Result<Vec<IndexBlob>> {
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
    // Reject a hostile or corrupt blob count BEFORE reserving for it: a frame holds
    // at most one blob per 4-byte length prefix, so a count the remaining bytes
    // cannot satisfy would otherwise drive `Vec::with_capacity` to request a
    // multi-gigabyte allocation (which aborts the process) off bytes a remote server
    // controls. `fetch_index` runs this on every remote add / sync / reconnect.
    if n > (bytes.len() - pos) / 4 {
        bail!(
            "framed index claims {n} blobs, more than the remaining {} bytes can hold",
            bytes.len() - pos
        );
    }
    let mut blobs = Vec::with_capacity(n);
    for _ in 0..n {
        let len = read_u32(bytes, &mut pos)? as usize;
        let blob: IndexBlob = bytes
            .get(pos..pos + len)
            .context("framed index truncated: expected a blob")?
            .into();
        pos += len;
        blobs.push(blob);
    }
    Ok(blobs)
}

/// Probe `server`'s health with one unauthenticated `GET /v0/ping`, timing
/// the round-trip. Offline is a VALUE, not an error: this never panics and
/// never returns `Err`, any failure (connect refused, timeout, a non-2xx
/// status, an unparsable body) reports `online: false` with every other
/// field `None` except `last_checked_unix` (always set, to `now_unix`), so a
/// caller can always render a row instead of handling a `Result`. Bounded to
/// a short (5 s) timeout so one unreachable remote can't stall a caller that
/// probes several.
pub fn probe_health(server: &RemoteServer, now_unix: u64) -> RemoteHealth {
    let offline = || RemoteHealth {
        online: false,
        latency_ms: None,
        server_version: None,
        index_version: None,
        last_checked_unix: Some(now_unix),
    };

    let Ok(client) = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    else {
        return offline();
    };

    let endpoint = format!("{}/v0/ping", server.url);
    let started = std::time::Instant::now();
    let response = match client.get(&endpoint).send() {
        Ok(r) => r,
        Err(_) => return offline(),
    };
    // Measured right after the response headers arrive, the round-trip,
    // not including the (negligible, local) JSON-parse below.
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    if !response.status().is_success() {
        return offline();
    }
    let Ok(body) = response.json::<serde_json::Value>() else {
        return offline();
    };

    RemoteHealth {
        online: true,
        latency_ms: Some(latency_ms),
        server_version: body["server"].as_str().map(str::to_string),
        index_version: body["index_version"].as_u64(),
        last_checked_unix: Some(now_unix),
    }
}

// ---------------------------------------------------------------------------
// OAuth2.1 Authorization-Code + PKCE login (RFC 8252 native-app flow)
// ---------------------------------------------------------------------------

/// A fresh bearer token pair minted by the server's `/token` endpoint:
/// `access` is the short-lived JWT sent as `Authorization: Bearer` on every
/// `/v0/*` call (held OPAQUELY, this client never parses or validates it,
/// it only ever forwards it); `refresh` is the long-lived, rotating token
/// [`TokenStore`] persists so a later session can skip the interactive
/// browser login via [`refresh`].
pub struct Tokens {
    /// The short-lived access JWT, sent as `Authorization: Bearer` (held opaquely).
    pub access: String,
    /// The long-lived rotating refresh token, persisted to skip the next browser login.
    pub refresh: String,
}

/// Fetch `{url}/.well-known/openid-configuration` and pull out the two
/// endpoints this client needs: where to send the user's browser
/// (`authorization_endpoint`) and where to redeem a code/refresh token
/// (`token_endpoint`).
fn discover(url: &str) -> anyhow::Result<(String, String)> {
    let endpoint = format!("{url}/.well-known/openid-configuration");
    let response = http_client(HTTP_REQUEST_TIMEOUT)
        .get(&endpoint)
        .send()
        .with_context(|| format!("GET {endpoint}"))?;
    if !response.status().is_success() {
        bail!("GET {endpoint} failed: {}", response.status());
    }

    let body: serde_json::Value = response
        .json()
        .with_context(|| format!("parsing discovery document from {endpoint}"))?;
    let authorization_endpoint = body["authorization_endpoint"]
        .as_str()
        .context("discovery document had no string `authorization_endpoint`")?
        .to_string();
    let token_endpoint = body["token_endpoint"]
        .as_str()
        .context("discovery document had no string `token_endpoint`")?
        .to_string();
    Ok((authorization_endpoint, token_endpoint))
}

/// `n` CSPRNG bytes via `getrandom` (the OS random source, no `rand` crate
/// needed for this).
fn random_bytes(n: usize) -> anyhow::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    // `getrandom::Error` doesn't implement `std::error::Error` under this
    // crate's default (no `std` feature) build, so `anyhow::Context` isn't
    // available on it, fold it into an anyhow error via `Display` instead.
    getrandom::getrandom(&mut buf)
        .map_err(|e| anyhow::anyhow!("reading CSPRNG bytes for OAuth/PKCE: {e}"))?;
    Ok(buf)
}

/// A fresh PKCE verifier/challenge pair (RFC 7636 §4.1/§4.2): `verifier` is
/// base64url-no-pad of 32 random bytes; `challenge` is
/// base64url-no-pad(SHA256(verifier)) (the `S256` method, the only one this
/// server, or OAuth 2.1, supports).
fn pkce_pair() -> anyhow::Result<(String, String)> {
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes(32)?);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}

/// A fresh CSRF `state` value: base64url-no-pad of 16 random bytes.
fn random_state() -> anyhow::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(16)?))
}

/// Percent-encodes `s` for safe inclusion as a query-string value (RFC 3986
/// unreserved set passes through unescaped; everything else becomes `%XX`).
/// Mirrors the server's own `percent_encode`, written by hand here too
/// rather than pulling in a URL crate for the one value (`redirect_uri`)
/// this client ever needs to escape.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Builds the `/authorize` URL the system browser is pointed at: `challenge`
/// and `state` are both already base64url (the unreserved alphabet, so they
/// need no escaping); `redirect_uri` is the one value that does.
fn authorize_url(auth_endpoint: &str, challenge: &str, state: &str, redirect_uri: &str) -> String {
    format!(
        "{auth_endpoint}?response_type=code&client_id=soul-player&redirect_uri={}\
         &code_challenge={challenge}&code_challenge_method=S256&state={state}&scope=library",
        percent_encode(redirect_uri)
    )
}

/// Extract `key`'s value out of `url_or_query` (either a full URL or a bare
/// `a=1&b=2` query string), percent-decoding simple `%XX` escapes. `None` if
/// `key` isn't present.
fn query_param(url_or_query: &str, key: &str) -> Option<String> {
    let query = url_or_query
        .split_once('?')
        .map_or(url_or_query, |(_, q)| q);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

/// Decodes `%XX` escapes in a query-string value. Any malformed escape (a
/// stray `%` not followed by two hex digits) is passed through unchanged
/// rather than erroring, this only ever reads values this client or the
/// trusted `soul-server` AS produced.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// POST `token_endpoint` with `form` and parse a successful response into
/// [`Tokens`]. Shared by [`exchange_code`] and [`refresh`], the only
/// difference between the two grants is which form fields they send.
fn post_token(token_endpoint: &str, form: &[(&str, &str)]) -> anyhow::Result<Tokens> {
    let response = http_client(HTTP_REQUEST_TIMEOUT)
        .post(token_endpoint)
        .form(form)
        .send()
        .with_context(|| format!("POST {token_endpoint}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        bail!("POST {token_endpoint} failed: {status} {body}");
    }

    let body: serde_json::Value = response
        .json()
        .with_context(|| format!("parsing token response from {token_endpoint}"))?;
    let access = body["access_token"]
        .as_str()
        .context("token response had no string `access_token`")?
        .to_string();
    let refresh = body["refresh_token"]
        .as_str()
        .context("token response had no string `refresh_token`")?
        .to_string();
    Ok(Tokens { access, refresh })
}

/// Redeem an authorization `code` (and its PKCE `verifier`) for a fresh
/// [`Tokens`] pair. The testable seam: everything [`login`] does AFTER the
/// browser/loopback dance, independently exercisable against a real AS
/// without a socket or a browser.
pub fn exchange_code(
    url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> anyhow::Result<Tokens> {
    let (_, token_endpoint) = discover(url)?;
    post_token(
        &token_endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("client_id", "soul-player"),
        ],
    )
}

/// Redeem a stored refresh token for a fresh [`Tokens`] pair. The server
/// rotates refresh tokens on every use (see `soul_server::auth::refresh`),
/// so the caller MUST persist the returned `refresh`, not just `access`.
pub fn refresh(url: &str, refresh: &str) -> anyhow::Result<Tokens> {
    let (_, token_endpoint) = discover(url)?;
    post_token(
        &token_endpoint,
        &[("grant_type", "refresh_token"), ("refresh_token", refresh)],
    )
}

/// Overall time this client waits for the user to finish the browser login
/// before giving up, generous (a password manager, an SSO redirect, a
/// distracted human) but finite, so [`login`] can never hang forever.
const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Accepts exactly one connection on `listener` (polling with a timeout
/// rather than blocking forever, so a browser that never shows up can't hang
/// the caller), reads its HTTP request line, and pulls `code`/`state` out of
/// the `GET /callback?code=..&state=..` request, then writes back a
/// minimal `200 OK` so the browser tab shows a friendly confirmation instead
/// of hanging or erroring.
fn accept_callback(listener: &std::net::TcpListener) -> anyhow::Result<(String, String)> {
    use std::io::BufRead as _;

    listener
        .set_nonblocking(true)
        .context("setting the loopback listener non-blocking")?;
    let deadline = std::time::Instant::now() + CALLBACK_TIMEOUT;

    // Loop over EVERY connection until one actually carries the `code` query.
    // A real browser following the `302` to this loopback sends exactly the
    // callback, but browsers also probe loopback redirect targets (a
    // speculative preconnect, a `/favicon.ico`, an `OPTIONS`), and if we
    // consumed the first such probe and returned, the ACTUAL callback would
    // then hit a closed listener and the user would see "can't connect". So we
    // answer-and-ignore anything without a `code` and keep waiting.
    loop {
        let mut stream = match listener.accept() {
            Ok((stream, _addr)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    bail!("timed out waiting for the browser login to complete");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e).context("accepting the OAuth callback connection"),
        };
        stream
            .set_nonblocking(false)
            .context("setting the accepted OAuth callback connection blocking")?;

        // Read the request line ("GET /callback?code=..&state=.. HTTP/1.1").
        // A broken/empty probe connection is ignored, not fatal.
        let path = {
            let mut reader = std::io::BufReader::new(&stream);
            let mut request_line = String::new();
            match reader.read_line(&mut request_line) {
                Ok(0) | Err(_) => None,
                Ok(_) => request_line
                    .split_whitespace()
                    .nth(1)
                    .map(ToString::to_string),
            }
        };

        match path
            .as_deref()
            .and_then(|p| Some((query_param(p, "code")?, query_param(p, "state")?)))
        {
            Some((code, state)) => {
                write_callback_response(
                    &mut stream,
                    "200 OK",
                    "You can close this tab and return to soul-player.",
                );
                return Ok((code, state));
            }
            None => {
                // Not the callback (favicon / preconnect / OPTIONS). Answer it
                // politely and keep waiting for the real redirect.
                write_callback_response(&mut stream, "404 Not Found", "not found");
            }
        }
    }
}

/// Write a minimal plain-text HTTP response to a loopback-callback connection.
/// Best-effort: a write failure on a throwaway probe socket must not abort the
/// login (the caller keeps waiting for the real callback).
fn write_callback_response(stream: &mut std::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    let _ = stream.write_all(response.as_bytes());
}

/// The full interactive login: discover the AS, bind a LOOPBACK-only
/// listener (never `0.0.0.0`, RFC 8252's native-app redirect is only ever
/// safe to accept from the local machine), build the PKCE+state
/// `/authorize` URL, open it in the system browser, accept the single
/// resulting callback, verify `state` (a CSRF guard, see RFC 6749 §10.12),
/// and exchange the code for [`Tokens`].
///
/// A thin wrapper over [`login_with_opener`] that opens the URL in the
/// user's actual system browser, see that function for the real logic and
/// for how tests exercise it without a browser.
pub fn login(url: &str) -> anyhow::Result<Tokens> {
    login_with_opener(url, |u| open::that(u).map_err(anyhow::Error::from))
}

/// [`login`]'s real logic, with how the `/authorize` URL gets opened
/// factored out behind `open_fn` so tests can stand in for the system
/// browser: a test's `open_fn` typically spawns a thread that plays browser
/// (GET the login form, POST credentials, follow the redirect back to this
/// function's OWN loopback listener) and returns immediately, exactly like
/// [`open::that`] does, `login_with_opener` can't call `open_fn` and THEN
/// `accept()`, waiting for a return that never comes, if `open_fn` blocked
/// until the whole browser flow finished.
pub(crate) fn login_with_opener(
    url: &str,
    open_fn: impl FnOnce(&str) -> anyhow::Result<()>,
) -> anyhow::Result<Tokens> {
    let (auth_endpoint, _) = discover(url)?;

    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("binding an ephemeral loopback port for the OAuth callback")?;
    let port = listener
        .local_addr()
        .context("reading the bound loopback port")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let (verifier, challenge) = pkce_pair()?;
    let state = random_state()?;
    let open_url = authorize_url(&auth_endpoint, &challenge, &state, &redirect_uri);

    open_fn(&open_url).context("opening the system browser for login")?;

    let (code, returned_state) = accept_callback(&listener)?;
    if returned_state != state {
        bail!("OAuth callback state mismatch, possible CSRF, aborting login");
    }

    exchange_code(url, &code, &verifier, &redirect_uri)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use soul_audio_core::Decoder;
    use soul_server::auth::signer::Signer;
    use soul_server::domain::UserId;
    use soul_server::domain::ports::{IndexPort, UserStore};
    use soul_server::domain::service::Services;
    use soul_server::index::CoreIndex;
    use soul_server::media::FsMedia;
    use soul_server::proto::native::{self, AppServices, AppState};
    use soul_server::store::FileUsers;

    use super::*;

    #[test]
    fn token_store_round_trips_save_load_delete() {
        let store = MemStore::new();
        assert_eq!(store.load_refresh("http://server-a"), None);

        store
            .save_refresh("http://server-a", "refresh-a")
            .expect("save should succeed");
        assert_eq!(
            store.load_refresh("http://server-a"),
            Some("refresh-a".to_string())
        );

        store
            .delete_refresh("http://server-a")
            .expect("delete should succeed");
        assert_eq!(store.load_refresh("http://server-a"), None);
    }

    #[test]
    fn token_store_keeps_different_server_urls_independent() {
        let store = MemStore::new();
        store
            .save_refresh("http://server-a", "refresh-a")
            .expect("save a");
        store
            .save_refresh("http://server-b", "refresh-b")
            .expect("save b");

        assert_eq!(
            store.load_refresh("http://server-a"),
            Some("refresh-a".to_string())
        );
        assert_eq!(
            store.load_refresh("http://server-b"),
            Some("refresh-b".to_string())
        );

        store.delete_refresh("http://server-a").expect("delete a");
        assert_eq!(store.load_refresh("http://server-a"), None);
        assert_eq!(
            store.load_refresh("http://server-b"),
            Some("refresh-b".to_string()),
            "deleting one server's token must not disturb another's"
        );
    }

    #[test]
    fn token_store_load_of_unknown_url_is_none() {
        let store = MemStore::new();
        assert_eq!(store.load_refresh("http://never-saved"), None);
    }

    #[test]
    fn token_store_delete_of_unknown_url_is_not_an_error() {
        let store = MemStore::new();
        assert!(store.delete_refresh("http://never-saved").is_ok());
    }

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
        signer: Arc<Signer>,
        user_id: UserId,
        // Kept alive so the server's worker threads (and listening socket)
        // keep running for the test's duration; never read directly.
        _runtime: tokio::runtime::Runtime,
        _dir: tempfile::TempDir,
    }

    impl TestServer {
        /// Mint a fresh, valid JWT access token for this server's seeded test
        /// user directly off its `Signer`, the cheap stand-in for a full
        /// browser/OAuth round-trip in tests that only need a VALID
        /// `access_token` on a [`RemoteServer`], not to exercise login itself
        /// (that's [`oauth_code_exchange_and_refresh`]'s job).
        fn token(&self) -> String {
            self.signer
                .mint(self.user_id, 3600, now_unix())
                .expect("mint a test access token")
        }
    }

    /// Seconds since the Unix epoch, for [`Signer::mint`]'s `now_unix`.
    fn now_unix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before the Unix epoch")
            .as_secs()
    }

    fn start_server() -> TestServer {
        start_server_over(vec![testdata_dir()])
    }

    /// Like [`start_server`], but scanning `roots` instead of the whole
    /// `testdata/` tree, so a test can stand up a server over a DIFFERENT
    /// subset of fixtures than whatever it scanned locally.
    ///
    /// Binds the ephemeral port SYNCHRONOUSLY, up front, rather than inside
    /// the spawned async task: the OAuth discovery document advertises
    /// `authorization_endpoint`/`token_endpoint` built from `issuer`, so
    /// `issuer` must be this server's actual bound `http://127.0.0.1:PORT`
    /// base, not a fixed placeholder, or a client that resolves endpoints
    /// via discovery (as [`exchange_code`]/[`refresh`]/[`login`] do) would
    /// be handed URLs with no port, to a server that isn't there.
    fn start_server_over(roots: Vec<PathBuf>) -> TestServer {
        let dir = tempfile::tempdir().expect("create temp dir");
        let index = CoreIndex::scan(roots).expect("scan the given roots");
        let users = FileUsers::open(&dir.path().join("users.json")).expect("open user store");
        let user_id = users.add_user("u", "p").expect("seed test user");
        let services = Arc::new(Services {
            index,
            media: FsMedia,
            users,
        });

        let std_listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        std_listener
            .set_nonblocking(true)
            .expect("set the bound listener non-blocking for tokio");
        let addr = std_listener.local_addr().expect("bound socket has an addr");
        let issuer = format!("http://{addr}");

        let signer_dir = dir.path().join("signer");
        let signer = Arc::new(
            Signer::load_or_create(&signer_dir, issuer.clone()).expect("load_or_create signer"),
        );
        let state = AppState::new(services.clone(), signer.clone(), issuer, 3600);

        let runtime = tokio::runtime::Runtime::new().expect("build tokio runtime");
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(std_listener)
                .expect("convert the bound std listener into a tokio listener");
            axum::serve(listener, native::router(state))
                .await
                .expect("serve the native router");
        });

        TestServer {
            url: format!("http://{addr}"),
            services,
            signer,
            user_id,
            _runtime: runtime,
            _dir: dir,
        }
    }

    #[test]
    fn fetch_index_returns_blobs_that_rebuild_a_nonempty_library() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
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
            access_token: "not-a-real-token".to_string(),
        };

        assert!(fetch_index(&remote).is_err());
    }

    #[test]
    fn download_cache_writes_matching_bytes_and_hits_cache_on_the_second_fetch() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
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
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        };

        let cache_dir = tempfile::tempdir().expect("create cache dir");
        let cache = DownloadCache {
            dir: cache_dir.path().to_path_buf(),
        };

        assert!(cache.fetch(&remote, 999_999).is_err());
    }

    /// Finds the first track id in `server`'s index whose original file is
    /// at least `min_len` bytes, used by the [`HttpRangeReader`] tests
    /// below to pick a track big enough to span more than one window, so
    /// reading it all the way through actually exercises the "window
    /// exhausted → refetch" path and not just the chunk primed at
    /// construction.
    fn find_track_at_least(server: &TestServer, min_len: u64) -> (u32, Vec<u8>) {
        (0u32..10_000)
            .find_map(|id| {
                let path = server.services.index.locate(id)?;
                let bytes = fs::read(&path).expect("read fixture file");
                (bytes.len() as u64 >= min_len).then_some((id, bytes))
            })
            .unwrap_or_else(|| {
                panic!("no track in the test server's index is at least {min_len} bytes")
            })
    }

    #[test]
    fn http_range_reader_streams_the_whole_track_byte_identical_to_a_full_download() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        };
        // Bigger than one window, so reading it start to finish also proves
        // the refetch-on-exhaustion path, not just the chunk primed at
        // construction.
        let (track_id, expected_bytes) = find_track_at_least(&server, CHUNK_SIZE + 1);

        let mut reader =
            HttpRangeReader::new(&remote, track_id).expect("construct HttpRangeReader");
        assert_eq!(
            reader.len(),
            expected_bytes.len() as u64,
            "len() should report the track's real size"
        );

        let mut got = Vec::new();
        reader
            .read_to_end(&mut got)
            .expect("read the whole stream sequentially");
        assert_eq!(
            got, expected_bytes,
            "streamed bytes should exactly match a full download of the same track"
        );
    }

    /// The read-ahead contract (the anti-dropout guarantee for remote playback):
    /// once a read crosses the current window's midpoint, the NEXT window is already
    /// being fetched on a background thread, so the read that eventually runs past
    /// the window joins a finished fetch instead of stalling the consuming (pump)
    /// thread on a network round-trip. And the stream stays byte-identical throughout.
    #[test]
    fn http_range_reader_reads_the_next_window_ahead_before_the_current_one_runs_out() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        };
        // Bigger than one window, so there IS a next window to read ahead.
        let (track_id, expected_bytes) = find_track_at_least(&server, CHUNK_SIZE + 1);
        let mut reader =
            HttpRangeReader::new(&remote, track_id).expect("construct HttpRangeReader");

        // Construction only primed window 0; nothing consumed yet, so no read-ahead.
        assert_eq!(
            reader.prefetch_start(),
            None,
            "a freshly opened reader should not have prefetched anything"
        );

        // Read past the midpoint of window 0 WITHOUT crossing its end.
        let head_len = (CHUNK_SIZE * 3 / 4) as usize;
        let mut head = vec![0u8; head_len];
        reader
            .read_exact(&mut head)
            .expect("read the head of window 0");
        assert_eq!(
            head,
            expected_bytes[..head_len],
            "window 0 bytes must be exact"
        );

        // Crossing the midpoint must have started reading window 1 ahead (the spawn
        // happens synchronously inside read(), so this is deterministic).
        assert_eq!(
            reader.prefetch_start(),
            Some(CHUNK_SIZE),
            "a read past the window midpoint should begin fetching the next window"
        );

        // End to end the stream is still byte-identical: the prefetched window
        // installs at the right offset with its data intact.
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).expect("read the remainder");
        let mut got = head;
        got.extend_from_slice(&rest);
        assert_eq!(
            got, expected_bytes,
            "read-ahead must not corrupt or misalign the streamed bytes"
        );
    }

    #[test]
    fn http_range_reader_seek_from_start_then_read_matches_the_slice() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        };
        let (track_id, expected_bytes) = find_track_at_least(&server, CHUNK_SIZE + 1);

        let mut reader =
            HttpRangeReader::new(&remote, track_id).expect("construct HttpRangeReader");

        // Seek past the window primed at construction, so this also proves a
        // seek-then-read refetches rather than silently serving stale bytes.
        let k = CHUNK_SIZE + 100;
        let n = 4096usize;
        let pos = reader
            .seek(SeekFrom::Start(k))
            .expect("seek to an absolute offset should succeed");
        assert_eq!(pos, k, "seek should report the new absolute position");

        let mut got = vec![0u8; n];
        reader
            .read_exact(&mut got)
            .expect("read N bytes after seeking");

        let k = k as usize;
        assert_eq!(
            got,
            expected_bytes[k..k + n],
            "bytes read after seek(Start(k)) should equal the file's [k..k+n) slice"
        );
    }

    #[test]
    fn http_range_reader_seek_from_end_lands_at_len_and_reads_zero_at_eof() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        };
        let expected_path = server
            .services
            .index
            .locate(0)
            .expect("track 0 resolves to a path");
        let expected_len = fs::metadata(&expected_path)
            .expect("stat the reference file")
            .len();

        let mut reader = HttpRangeReader::new(&remote, 0).expect("construct HttpRangeReader");
        assert_eq!(
            reader.len(),
            expected_len,
            "len() should match the real file size"
        );

        let pos = reader
            .seek(SeekFrom::End(0))
            .expect("seek to end should succeed without touching the network");
        assert_eq!(
            pos,
            reader.len(),
            "SeekFrom::End(0) should land exactly at len()"
        );

        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).expect("read at EOF should not error");
        assert_eq!(n, 0, "reading at EOF should return Ok(0), never an error");
    }

    #[test]
    fn http_range_reader_seek_before_the_start_errors() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        };
        let mut reader = HttpRangeReader::new(&remote, 0).expect("construct HttpRangeReader");

        assert!(
            reader.seek(SeekFrom::Current(-1)).is_err(),
            "seeking to a negative absolute position must be an error, not wrap/clamp"
        );
    }

    #[test]
    fn http_range_reader_new_for_an_unknown_track_errors() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        };

        assert!(HttpRangeReader::new(&remote, 999_999).is_err());
    }

    #[test]
    fn http_range_reader_rejects_a_bad_access_token() {
        // The streaming endpoint is authenticated too (like `fetch_index`): a
        // wrong/expired access token makes the range request return a non-206 status,
        // which `fetch_chunk` surfaces as an `Err` rather than silently opening a reader
        // over empty/garbage audio. Track 0 IS valid here, so this isolates the AUTH
        // rejection (distinct from `..._for_an_unknown_track_errors`). Note there is no
        // mid-stream token refresh, the reader holds the token captured at construction
        // (`access_token`), so an expired token is terminal for that reader.
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: "not-a-valid-token".to_string(),
        };
        assert!(
            HttpRangeReader::new(&remote, 0).is_err(),
            "a bad access token on the streaming endpoint must error, not open a reader"
        );
    }

    #[test]
    fn parse_content_range_total_reads_the_total_after_the_slash() {
        // The streamed track's length is learned by parsing the server's
        // `Content-Range: bytes START-END/TOTAL` header, the denominator `len()` keys
        // off. Valid forms yield the total; a malformed / unknown (`*`) / absent total
        // yields None, so `fetch_chunk` errors rather than guessing a size.
        assert_eq!(parse_content_range_total("bytes 0-1023/4096"), Some(4096));
        assert_eq!(parse_content_range_total("bytes 0-0/1"), Some(1));
        // The unknown-RANGE `*` form still carries a concrete total.
        assert_eq!(parse_content_range_total("bytes */1234"), Some(1234));
        // No slash, an unknown (`*`) total, or an empty value -> None.
        assert_eq!(parse_content_range_total("bytes 0-1023"), None);
        assert_eq!(parse_content_range_total("bytes 0-10/*"), None);
        assert_eq!(parse_content_range_total(""), None);
    }

    #[test]
    fn deframe_round_trips_with_the_servers_frame_index() {
        let blobs = vec![b"hello".to_vec(), Vec::new(), b"a bit longer blob".to_vec()];
        let framed = soul_server::index::frame_index(&blobs);

        let back = deframe(&framed).expect("deframe should parse what the server framed");
        assert_eq!(back.len(), blobs.len());
        for (got, want) in back.iter().zip(&blobs) {
            assert_eq!(got.as_ref(), want.as_slice());
        }
    }

    #[test]
    fn deframe_rejects_truncated_bytes() {
        let truncated = 1u32.to_le_bytes().to_vec();
        assert!(deframe(&truncated).is_err());
    }

    /// A hostile or corrupt blob-count header must be rejected cleanly (an `Err`),
    /// never fed to `Vec::with_capacity`: a near-`u32::MAX` count would otherwise
    /// reserve ~100 GiB and abort the process. `fetch_index` deframes bytes a remote
    /// server controls, so this runs on every remote add / sync / reconnect.
    #[test]
    fn deframe_rejects_an_implausible_blob_count() {
        // A count header of u32::MAX with no blob bytes following it.
        let hostile = u32::MAX.to_le_bytes().to_vec();
        assert!(deframe(&hostile).is_err());
    }

    /// A remote server that accepts the connection but never answers must not hang
    /// the calling thread: `http_client`'s total timeout has to fire. Guards the
    /// anti-hang contract against a bare `reqwest::blocking::Client::new()` (which
    /// has NO default timeout) creeping back into any remote call site.
    #[test]
    fn http_client_times_out_on_a_stalled_server() {
        use std::io::Read;
        use std::time::{Duration, Instant};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().expect("local addr");
        // Accept one connection, drain the request, then stall without ever sending
        // a response. Fire-and-forget: the client must give up on its own timeout
        // long before this thread's stall elapses.
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                let _ = sock.read(&mut buf);
                std::thread::sleep(Duration::from_secs(10));
            }
        });

        let client = http_client(Duration::from_millis(300));
        let start = Instant::now();
        let result = client.get(format!("http://{addr}/stall")).send();
        let elapsed = start.elapsed();

        assert!(
            result.is_err(),
            "a stalled server must surface an error, not hang"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "the 300ms-timeout client should give up quickly, took {elapsed:?}"
        );
    }

    /// Bake `root` (a single-root scan) and hand back its one `.soulidx`
    /// blob, reuses the server's own `CoreIndex`/framing so this is exactly
    /// the shape `RemoteLibrary::build`'s `local` and `fetch_index`'s remote
    /// blobs both take.
    fn scan_blob(root: &std::path::Path) -> IndexBlob {
        let index = CoreIndex::scan(vec![root.to_path_buf()]).expect("scan the given root");
        let framed = index.index_bytes().expect("frame the scanned index");
        let mut blobs = deframe(&framed).expect("deframe what we just framed");
        assert_eq!(blobs.len(), 1, "a single-root scan bakes one blob");
        blobs.remove(0)
    }

    /// Extract the value of `key` from `location`'s query string (used here
    /// to pull `code`/`state` back out of the server's `302 Location`
    /// header, the same parsing [`query_param`] does for the real loopback
    /// callback in [`login`]).
    fn location_param(location: &str, key: &str) -> Option<String> {
        query_param(location, key)
    }

    /// GETs `url` (the loopback callback) with a few short retries rather
    /// than one shot, tolerating the brief window between
    /// `login_with_opener` binding its `TcpListener` and actually reaching
    /// its `accept()` loop. In practice the listener is bound long before
    /// the browser-thread finishes the AS round-trip below, so this almost
    /// never retries; it exists so the test fails loudly on a real timeout
    /// instead of racily, if that ordering ever tightens.
    fn browser_get_loopback(client: &reqwest::blocking::Client, url: &str) {
        let mut last_err = None;
        for attempt in 0..20 {
            match client.get(url).send() {
                Ok(_) => return,
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 19 {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
        }
        panic!("GET {url} (the loopback callback) failed after retries: {last_err:?}");
    }

    /// Drives the full OAuth2.1 Authorization-Code+PKCE dance against a REAL
    /// in-process AS, standing in for the browser with a non-redirecting
    /// blocking client (so the test controls the `code`/`state` handoff
    /// instead of a real loopback socket): `GET`+`POST /authorize` to mint a
    /// code, [`exchange_code`] it for [`Tokens`] whose `access` the real
    /// `/v0/index` accepts, then [`refresh`] into a second [`Tokens`] whose
    /// `access` ALSO works and whose `refresh` has rotated.
    #[test]
    fn oauth_code_exchange_and_refresh() {
        let server = start_server();
        let redirect_uri = "http://127.0.0.1:1/callback".to_string();

        let (verifier, challenge) = pkce_pair().expect("generate a PKCE pair");
        let state = random_state().expect("generate a CSRF state");

        let browser = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build a non-redirecting client");

        let auth_endpoint = format!("{}/authorize", server.url);
        let get_url = authorize_url(&auth_endpoint, &challenge, &state, &redirect_uri);
        let get_resp = browser
            .get(&get_url)
            .send()
            .expect("GET /authorize should succeed");
        assert_eq!(
            get_resp.status(),
            reqwest::StatusCode::OK,
            "expected the login form"
        );

        let post_resp = browser
            .post(&auth_endpoint)
            .form(&[
                ("response_type", "code"),
                ("client_id", "soul-player"),
                ("redirect_uri", redirect_uri.as_str()),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("state", state.as_str()),
                ("scope", "library"),
                ("username", "u"),
                ("password", "p"),
            ])
            .send()
            .expect("POST /authorize should succeed");
        assert_eq!(
            post_resp.status(),
            reqwest::StatusCode::FOUND,
            "right credentials should redirect with a code"
        );
        let location = post_resp
            .headers()
            .get(reqwest::header::LOCATION)
            .expect("302 should carry a Location header")
            .to_str()
            .expect("Location header should be valid UTF-8")
            .to_string();

        let code = location_param(&location, "code").expect("Location should carry a code");
        let returned_state =
            location_param(&location, "state").expect("Location should carry state");
        assert_eq!(returned_state, state, "state must round-trip unchanged");

        let tokens = exchange_code(&server.url, &code, &verifier, &redirect_uri)
            .expect("exchange_code should succeed with a valid code+verifier");
        assert!(!tokens.access.is_empty());
        assert!(!tokens.refresh.is_empty());

        let index_resp = reqwest::blocking::Client::new()
            .get(format!("{}/v0/index", server.url))
            .bearer_auth(&tokens.access)
            .send()
            .expect("GET /v0/index");
        assert_eq!(
            index_resp.status(),
            reqwest::StatusCode::OK,
            "the exchanged access token should be accepted by /v0/index"
        );

        let refreshed = refresh(&server.url, &tokens.refresh)
            .expect("refresh should succeed with a valid refresh token");
        assert!(!refreshed.access.is_empty());
        assert_ne!(
            refreshed.refresh, tokens.refresh,
            "refresh tokens must rotate on use"
        );

        let index_resp2 = reqwest::blocking::Client::new()
            .get(format!("{}/v0/index", server.url))
            .bearer_auth(&refreshed.access)
            .send()
            .expect("GET /v0/index with the refreshed token");
        assert_eq!(
            index_resp2.status(),
            reqwest::StatusCode::OK,
            "the refreshed access token should also be accepted by /v0/index"
        );
    }

    /// Drives the same OAuth2.1 Authorization-Code+PKCE dance as
    /// [`oauth_code_exchange_and_refresh`] (GET+POST `/authorize` standing in
    /// for the browser, then [`exchange_code`]) against `server`'s seeded
    /// `"u"`/`"p"` user, returning the resulting [`Tokens`], the "get Tokens
    /// via the E2 flow" half of [`add_remote_source_with_tokens_lists_views`].
    fn login_as_test_user(server: &TestServer) -> Tokens {
        let redirect_uri = "http://127.0.0.1:1/callback".to_string();
        let (verifier, challenge) = pkce_pair().expect("generate a PKCE pair");
        let state = random_state().expect("generate a CSRF state");

        let browser = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build a non-redirecting client");

        let auth_endpoint = format!("{}/authorize", server.url);
        let get_url = authorize_url(&auth_endpoint, &challenge, &state, &redirect_uri);
        browser
            .get(&get_url)
            .send()
            .expect("GET /authorize should succeed");

        let post_resp = browser
            .post(&auth_endpoint)
            .form(&[
                ("response_type", "code"),
                ("client_id", "soul-player"),
                ("redirect_uri", redirect_uri.as_str()),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("state", state.as_str()),
                ("scope", "library"),
                ("username", "u"),
                ("password", "p"),
            ])
            .send()
            .expect("POST /authorize should succeed");
        assert_eq!(
            post_resp.status(),
            reqwest::StatusCode::FOUND,
            "right credentials should redirect with a code"
        );
        let location = post_resp
            .headers()
            .get(reqwest::header::LOCATION)
            .expect("302 should carry a Location header")
            .to_str()
            .expect("Location header should be valid UTF-8")
            .to_string();

        let code = location_param(&location, "code").expect("Location should carry a code");
        let returned_state =
            location_param(&location, "state").expect("Location should carry state");
        assert_eq!(returned_state, state, "state must round-trip unchanged");

        exchange_code(&server.url, &code, &verifier, &redirect_uri)
            .expect("exchange_code should succeed with a valid code+verifier")
    }

    /// The E3 seam, end to end: a real OAuth login against an in-process AS
    /// hands back [`Tokens`] whose `access` [`PlayerHandle::add_remote_source_with_tokens`]
    /// carries straight into `fetch_index`, no browser, no OS keychain,
    /// and the synced catalog shows up in [`PlayerHandle::views`].
    #[test]
    fn add_remote_source_with_tokens_lists_views() {
        let server = start_server();
        let tokens = login_as_test_user(&server);

        let handle = crate::PlayerHandle::offline();
        handle
            .add_remote_source_with_tokens(&server.url, tokens)
            .expect("add remote source with tokens: fetch the index, fold it into the library");

        assert!(
            !handle.views().is_empty(),
            "the authed fetch_index should have populated the aggregate library"
        );
    }

    /// [`probe_health`] against a REAL live server: online, a measured
    /// latency, the server's own crate version (echoed by `/v0/ping`), and
    /// its current index version.
    #[test]
    fn probe_health_reports_online_latency_and_version() {
        let server = start_server();
        let remote = RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        };

        let health = probe_health(&remote, now_unix());

        assert!(health.online, "a live server should report online");
        assert!(
            health.latency_ms.is_some(),
            "an online probe should report a measured latency"
        );
        assert_eq!(
            health.server_version,
            Some(soul_server::version().to_string()),
            "/v0/ping echoes the server crate's own version"
        );
        assert!(
            health.index_version.is_some(),
            "an online probe should report the server's index version"
        );
        assert!(
            health.last_checked_unix.is_some(),
            "the probe timestamp is always recorded, online or not"
        );
    }

    /// [`probe_health`] against a URL nothing is listening on: offline, with
    /// no latency or version, never an `Err`/panic, offline is a value.
    #[test]
    fn probe_health_reports_offline_for_dead_url() {
        // Bind an ephemeral port, then drop the listener immediately: the
        // port is very likely still free (nothing else grabs it in the
        // instant between bind and drop), but nothing answers on it anymore.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        let port = listener.local_addr().expect("bound addr").port();
        drop(listener);

        let remote = RemoteServer {
            url: format!("http://127.0.0.1:{port}"),
            access_token: "irrelevant".to_string(),
        };

        let health = probe_health(&remote, now_unix());

        assert!(!health.online, "nothing is listening, must report offline");
        assert!(health.latency_ms.is_none(), "no latency without a response");
        assert!(
            health.server_version.is_none(),
            "no version without a response"
        );
        assert!(
            health.index_version.is_none(),
            "no index version without a response"
        );
        assert!(
            health.last_checked_unix.is_some(),
            "the probe timestamp is always recorded, online or not"
        );
    }

    /// The Inc-1 surfacing contract, end to end: a remote added via
    /// [`crate::PlayerHandle::add_remote_source_with_tokens`] shows up in
    /// [`crate::PlayerHandle::sources`] (after a
    /// [`crate::PlayerHandle::refresh_remote_health`] probe) with
    /// `kind == Remote { url }` and online health, alongside any configured
    /// local folder which must keep `kind == Local` and `remote == None`.
    #[test]
    fn sources_lists_remote_with_kind_and_health() {
        let server = start_server();
        let tokens = login_as_test_user(&server);

        let handle = crate::PlayerHandle::offline();
        handle
            .set_sources(&[(testdata_dir(), true)])
            .expect("seed a local source");
        handle
            .add_remote_source_with_tokens(&server.url, tokens)
            .expect("add remote source with tokens");

        // `refresh_remote_health` does the network probe (off the UI thread
        // in a real caller) and hands back the fresh `sources()`.
        let sources = handle.refresh_remote_health();

        let remote_status = sources
            .iter()
            .find(|s| matches!(&s.kind, crate::SourceKind::Remote { url } if url == &server.url))
            .expect("sources() should list the added remote");
        let health = remote_status
            .remote
            .clone()
            .expect("a Remote source always carries Some(health)");
        assert!(health.online, "a live server should report online");
        assert!(
            remote_status.track_count > 0,
            "the synced remote catalog should contribute tracks"
        );

        let local_status = sources
            .iter()
            .find(|s| {
                s.kind
                    == crate::SourceKind::Local {
                        path: testdata_dir(),
                    }
            })
            .expect("sources() should still list the configured local folder");
        assert!(
            local_status.remote.is_none(),
            "a Local source must carry remote == None"
        );
    }

    /// The Inc-3 library marker, end to end: `views()` stamps
    /// [`crate::TrackView::remote_source`] from the aggregate's compound id,
    /// using the SAME sentinel-root convention
    /// `sources_lists_remote_with_kind_and_health` already proved for
    /// `track_count`, a remote server's tracks come back
    /// `remote_source == Some(host)` (a non-empty host/label naming THAT
    /// server, not just a bare flag), the configured local folder's stay
    /// `remote_source == None`.
    #[test]
    fn views_marks_remote_tracks_with_their_source_host_and_local_tracks_none() {
        let server = start_server();
        let tokens = login_as_test_user(&server);

        let handle = crate::PlayerHandle::offline();
        handle
            .set_sources(&[(testdata_dir(), true)])
            .expect("seed a local source");
        handle
            .add_remote_source_with_tokens(&server.url, tokens)
            .expect("add remote source with tokens");

        let views = handle.views();
        assert!(!views.is_empty(), "the aggregate should list tracks");

        let remote_hosts: Vec<&str> = views
            .iter()
            .filter_map(|v| v.remote_source.as_deref())
            .collect();
        assert!(
            !remote_hosts.is_empty(),
            "the synced remote catalog should contribute at least one track with a remote_source"
        );
        assert!(
            remote_hosts.iter().all(|h| !h.is_empty()),
            "every remote_source host should be non-empty: {remote_hosts:?}"
        );
        assert!(
            views.iter().any(|v| v.remote_source.is_none()),
            "the configured local folder should still contribute at least one track with remote_source == None"
        );
    }

    /// The Inc-5 reconnect seam, end to end: a [`MemStore`] pre-seeded with a
    /// valid refresh token (obtained via the same [`login_as_test_user`] flow
    /// [`add_remote_source_with_tokens_lists_views`] uses) lets
    /// [`crate::PlayerHandle::reconnect_remote_source`] silently re-authenticate
    /// against a REAL in-process AS, no browser, no OS keychain, fetch the
    /// catalog index, and fold it into the library, exactly like
    /// `add_remote_source_with_tokens` does. Also proves the server's refresh-
    /// token ROTATION is persisted back into the store, not just the access
    /// token, a caller that kept the old refresh token would be unable to
    /// reconnect a second time.
    #[test]
    fn reconnect_remote_source_rotates_token_and_rebuilds_library() {
        let server = start_server();
        let tokens = login_as_test_user(&server);

        let store = MemStore::new();
        store
            .save_refresh(&server.url, &tokens.refresh)
            .expect("seed the store with a valid refresh token");

        let handle = crate::PlayerHandle::offline();
        handle
            .reconnect_remote_source(&server.url, &store)
            .expect("reconnect should succeed with a valid stored refresh token");

        assert!(
            !handle.views().is_empty(),
            "a successful reconnect should populate the aggregate library"
        );
        let rotated = store
            .load_refresh(&server.url)
            .expect("the store should hold a refresh token after a successful reconnect");
        assert_ne!(
            rotated, tokens.refresh,
            "the refresh token must have rotated, the seeded one is now spent"
        );
    }

    /// A remote with no stored credential (never logged in, or the entry was
    /// deleted) fails clearly rather than reaching the network at all, the
    /// "no stored credential" `anyhow::bail!` path, which lets a caller tell
    /// this apart from a transient network failure if it ever wants to.
    #[test]
    fn reconnect_remote_source_without_a_stored_credential_errors() {
        let handle = crate::PlayerHandle::offline();
        let store = MemStore::new();

        let err = handle
            .reconnect_remote_source("http://example.invalid", &store)
            .expect_err("reconnecting with no stored refresh token must fail");
        assert!(
            err.to_string().contains("no stored credential"),
            "unexpected error message: {err}"
        );
    }

    /// [`crate::PlayerHandle::remove_remote_source`]: a remote added via
    /// `add_remote_source_with_tokens` shows up in [`crate::PlayerHandle::sources`]
    /// with `kind == Remote { url }`; removing it drops that entry (and the
    /// aggregate rebuilds without its tracks) while leaving everything else,
    /// here, nothing else configured, unaffected.
    #[test]
    fn remove_remote_source_drops_it_from_sources_and_rebuilds() {
        let server = start_server();
        let tokens = login_as_test_user(&server);

        let handle = crate::PlayerHandle::offline();
        handle
            .add_remote_source_with_tokens(&server.url, tokens)
            .expect("add remote source with tokens");
        assert!(
            handle.sources().iter().any(
                |s| matches!(&s.kind, crate::SourceKind::Remote { url } if url == &server.url)
            ),
            "the added remote should be listed before removal"
        );
        assert!(
            !handle.views().is_empty(),
            "the remote's tracks should be live"
        );

        handle.remove_remote_source(&server.url);

        assert!(
            !handle.sources().iter().any(
                |s| matches!(&s.kind, crate::SourceKind::Remote { url } if url == &server.url)
            ),
            "the removed remote must no longer be listed"
        );
        assert!(
            handle.views().is_empty(),
            "with no other source configured, removing the only remote should empty the library"
        );
    }

    /// Re-adding an already-configured remote (exactly what a Rescan / the
    /// periodic re-sync does) REPLACES its entry rather than appending a second
    /// one, the guard for the user-facing "rescan duplicated the remote" bug.
    /// After two adds of the same url the aggregate lists exactly ONE remote
    /// source, and its catalog is present once, not doubled.
    #[test]
    fn re_adding_a_remote_dedupes_to_one_source() {
        let server = start_server();
        let handle = crate::PlayerHandle::offline();

        handle
            .add_remote_source_with_tokens(&server.url, login_as_test_user(&server))
            .expect("first add");
        let first_tracks = handle.views().len();
        assert!(first_tracks > 0, "the first add should populate tracks");

        // A re-sync: the same url added again (fresh tokens, freshly-fetched
        // index), the way reconnect_remote_source feeds it back in.
        handle
            .add_remote_source_with_tokens(&server.url, login_as_test_user(&server))
            .expect("re-add (re-sync) the same url");

        let remote_rows = handle
            .sources()
            .iter()
            .filter(|s| matches!(&s.kind, crate::SourceKind::Remote { url } if url == &server.url))
            .count();
        assert_eq!(
            remote_rows, 1,
            "re-adding the same url must not duplicate the source row"
        );
        assert_eq!(
            handle.views().len(),
            first_tracks,
            "the re-synced catalog holds the same tracks once, not doubled"
        );
    }

    /// Disabling a connected remote keeps its row listed but drops its tracks
    /// from the aggregate (mirroring a disabled local folder); re-enabling
    /// restores them from the already-fetched blobs with no network call, the
    /// remote enable/disable toggle the Sources panel exposes.
    #[test]
    fn disabling_a_remote_drops_its_tracks_but_keeps_the_row() {
        let server = start_server();
        let handle = crate::PlayerHandle::offline();
        handle
            .add_remote_source_with_tokens(&server.url, login_as_test_user(&server))
            .expect("add remote");
        let enabled_tracks = handle.views().len();
        assert!(enabled_tracks > 0, "the enabled remote contributes tracks");

        handle
            .set_remote_enabled(&server.url, false)
            .expect("disable the remote");
        assert!(
            handle.sources().iter().any(
                |s| matches!(&s.kind, crate::SourceKind::Remote { url } if url == &server.url)
            ),
            "a disabled remote stays listed"
        );
        assert!(
            handle.views().is_empty(),
            "a disabled remote contributes no tracks to the aggregate"
        );

        handle
            .set_remote_enabled(&server.url, true)
            .expect("re-enable the remote");
        assert_eq!(
            handle.views().len(),
            enabled_tracks,
            "re-enabling restores the remote's tracks from the already-fetched blobs"
        );
    }

    /// Playlists store a durable `TrackLocator` per entry, not the volatile
    /// compound id. This pins the REMOTE case end to end (playlists + remotes
    /// compose): a remote track's locator round-trips to its current id while
    /// the remote is online, resolves to `None` (→ the playlist UI's greyed
    /// "unavailable" row) when the remote is disabled/offline rather than a
    /// WRONG id, and comes back on re-enable. Durability across a source-order
    /// shift is what keeps a remote track in a playlist from silently pointing
    /// at a different song.
    #[test]
    fn a_remote_track_locator_round_trips_and_greys_out_when_the_remote_is_disabled() {
        let server = start_server();
        let handle = crate::PlayerHandle::offline();
        handle
            .add_remote_source_with_tokens(&server.url, login_as_test_user(&server))
            .expect("add remote");

        let remote_id = handle
            .views()
            .iter()
            .find(|t| t.remote_source.is_some())
            .map(|t| t.id)
            .expect("the remote source contributed at least one track");

        // What a playlist would store when you add this remote track.
        let locator = handle
            .locator_of(remote_id)
            .expect("a remote track must yield a durable locator (not None)");

        // Online: the stored locator round-trips back to the current id.
        assert_eq!(
            handle.resolve_locators(std::slice::from_ref(&locator)),
            vec![Some(remote_id)],
            "a remote locator resolves to the current id while the remote is online"
        );

        // Disabled: the remote leaves the aggregate, so its locator resolves to
        // None, the playlist renders it greyed/unavailable, NOT as a wrong id.
        handle
            .set_remote_enabled(&server.url, false)
            .expect("disable the remote");
        assert_eq!(
            handle.resolve_locators(std::slice::from_ref(&locator)),
            vec![None],
            "a disabled remote's locator resolves to None (greyed/unavailable), never a wrong id"
        );

        // Re-enabled: it resolves again.
        handle
            .set_remote_enabled(&server.url, true)
            .expect("re-enable the remote");
        assert_eq!(
            handle.resolve_locators(std::slice::from_ref(&locator)),
            vec![Some(remote_id)],
            "re-enabling the remote restores resolution"
        );
    }

    #[test]
    fn remote_library_resolves_local_and_remote_tracks_with_a_nonzero_base() {
        // A local source with exactly ONE track, so its single id is
        // unambiguous, followed (in the aggregate) by a remote block, this
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
        let remote_server = Arc::new(RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
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
        // before them), a nonzero base. For each of the server's own track
        // ids, the aggregate id is `pack(1, server_id)`; resolving it must
        // download bytes identical to what the server itself would hand back
        // for that SAME server-side id, proving the (source, local) unpack
        // and repack, not a flat subtraction, recovered it correctly.
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

    /// [`RemoteLibrary::open_decoder`], end to end against a REAL in-process
    /// server: a LOCAL id delegates to `Library::open_decoder` (`open_track`,
    /// unchanged) and a REMOTE id decodes via the STREAMING path
    /// ([`HttpRangeReader`] + `open_seekable_src`), proven by decoding a real
    /// block from each AND by the download cache staying completely empty
    /// afterwards, since the happy streaming path never touches
    /// [`DownloadCache`] (the fallback it would otherwise fall back to).
    #[test]
    fn remote_library_open_decoder_streams_remote_and_delegates_local() {
        let local_dir = tempfile::tempdir().expect("local dir");
        fs::copy(
            testdata_dir().join("meta.flac"),
            local_dir.path().join("meta.flac"),
        )
        .expect("copy local fixture");
        let local_root = local_dir.path().to_path_buf();
        let local_blob = scan_blob(&local_root);

        let remote_src_dir = tempfile::tempdir().expect("remote dir");
        fs::copy(
            testdata_dir().join("tone.flac"),
            remote_src_dir.path().join("tone.flac"),
        )
        .expect("copy remote fixture");
        let server = start_server_over(vec![remote_src_dir.path().to_path_buf()]);
        let remote_server = Arc::new(RemoteServer {
            url: server.url.clone(),
            access_token: server.token(),
        });
        let remote_blobs = fetch_index(&remote_server).expect("fetch_index");

        let cache_dir = tempfile::tempdir().expect("download cache dir");
        let fetch: Arc<dyn MediaFetch> = Arc::new(DownloadCache {
            dir: cache_dir.path().to_path_buf(),
        });
        let remote_lib = Arc::new(RemoteLibrary::build(
            vec![(local_root, local_blob)],
            vec![(Arc::clone(&remote_server), remote_blobs)],
            fetch,
        ));

        // `OwnedDecoder` is large by value (the boxed `AnyDecoder`), a
        // big-stack thread, the same precaution `open_track`'s own decode
        // tests (and `remote_e2e.rs`) take for debug-build move frames.
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let mut buf = [0i32; 4096];

                // The single local source (index 0) still decodes through the
                // ordinary `open_track` path.
                let local_id = pack(0, 0);
                let mut local_decoder = remote_lib
                    .open_decoder(local_id)
                    .expect("local id should open a decoder");
                let n = local_decoder
                    .decode_into(&mut buf)
                    .expect("decode the local track's first block");
                assert!(n > 0, "expected a non-empty decoded block (local)");

                // The remote block starts at source 1 (base 1, the single
                // local source before it) and STREAMS over HTTP Range.
                let remote_id = pack(1, 0);
                let mut remote_decoder = remote_lib
                    .open_decoder(remote_id)
                    .expect("remote id should open a STREAMING decoder");
                let n = remote_decoder
                    .decode_into(&mut buf)
                    .expect("decode the remote track's first block");
                assert!(
                    n > 0,
                    "expected a non-empty decoded block (remote, streamed)"
                );
            })
            .expect("spawn decode thread")
            .join()
            .expect("decode thread panicked");

        // The happy streaming path never falls back to the download cache,
        // no file should have landed there.
        let cache_is_empty = fs::read_dir(cache_dir.path())
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true);
        assert!(
            cache_is_empty,
            "streaming a remote track must not fall back to downloading it first"
        );
    }

    /// Drives [`login_with_opener`] through its REAL bind→accept→parse→
    /// state-verify→exchange path, the one `login` itself has but no
    /// prior test exercised, since [`oauth_code_exchange_and_refresh`]
    /// calls [`exchange_code`] directly and never touches a loopback
    /// socket at all.
    ///
    /// The `open_fn` here stands in for `open::that`: it spawns a thread
    /// (so this call returns immediately, exactly like the real browser
    /// launcher does) that plays the browser, GET the login form POSTed
    /// to by the AS, POST the seeded user's credentials plus the echoed
    /// PKCE/state params, follow the resulting `302` BY HAND to its
    /// `Location`, and `GET` that loopback URL. That last GET is the one
    /// connection `login_with_opener`'s own `TcpListener` accepts, proving
    /// the real listener → request-line parse → `state` check → code
    /// exchange chain end to end.
    #[test]
    fn login_full_flow_catches_loopback_redirect() {
        let server = start_server();
        let server_url = server.url.clone();

        // The opener must return immediately (like `open::that`), so the
        // thread it spawns is handed back to the test via this slot rather
        // than joined inline, joined below, AFTER `login_with_opener`
        // returns, so a panic inside the browser-thread still fails the
        // test instead of vanishing silently.
        let handle_slot: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> =
            Arc::new(Mutex::new(None));
        let slot = Arc::clone(&handle_slot);

        let tokens = login_with_opener(&server_url, move |open_url| {
            let open_url = open_url.to_string();
            let handle = std::thread::spawn(move || {
                let browser = reqwest::blocking::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(5))
                    .connect_timeout(std::time::Duration::from_secs(5))
                    .build()
                    .expect("build a non-redirecting blocking client");

                let get_resp = browser
                    .get(&open_url)
                    .send()
                    .expect("GET the authorize URL (the login form) should succeed");
                assert_eq!(
                    get_resp.status(),
                    reqwest::StatusCode::OK,
                    "expected the login form"
                );

                let auth_endpoint = open_url
                    .split_once('?')
                    .map_or(open_url.as_str(), |(base, _)| base)
                    .to_string();
                let redirect_uri = query_param(&open_url, "redirect_uri")
                    .expect("authorize URL should carry redirect_uri");
                let challenge = query_param(&open_url, "code_challenge")
                    .expect("authorize URL should carry code_challenge");
                let state =
                    query_param(&open_url, "state").expect("authorize URL should carry state");

                let post_resp = browser
                    .post(&auth_endpoint)
                    .form(&[
                        ("response_type", "code"),
                        ("client_id", "soul-player"),
                        ("redirect_uri", redirect_uri.as_str()),
                        ("code_challenge", challenge.as_str()),
                        ("code_challenge_method", "S256"),
                        ("state", state.as_str()),
                        ("scope", "library"),
                        ("username", "u"),
                        ("password", "p"),
                    ])
                    .send()
                    .expect("POST /authorize should succeed");
                assert_eq!(
                    post_resp.status(),
                    reqwest::StatusCode::FOUND,
                    "right credentials should redirect with a code"
                );
                let location = post_resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .expect("302 should carry a Location header")
                    .to_str()
                    .expect("Location header should be valid UTF-8")
                    .to_string();

                // THIS is the connection login_with_opener's own
                // TcpListener accepts, the real loopback redirect catch.
                browser_get_loopback(&browser, &location);
            });
            *slot.lock().expect("lock the browser-thread handle slot") = Some(handle);
            Ok(())
        })
        .expect("login_with_opener should complete the real loopback flow");

        handle_slot
            .lock()
            .expect("lock the browser-thread handle slot")
            .take()
            .expect("opener should have stored a browser-thread handle")
            .join()
            .expect("browser thread should not panic");

        assert!(!tokens.access.is_empty());
        assert!(!tokens.refresh.is_empty());

        let index_resp = reqwest::blocking::Client::new()
            .get(format!("{server_url}/v0/index"))
            .bearer_auth(&tokens.access)
            .send()
            .expect("GET /v0/index");
        assert_eq!(
            index_resp.status(),
            reqwest::StatusCode::OK,
            "the access token from login_with_opener should be accepted by /v0/index"
        );
    }

    /// Regression for the "could not connect" loopback bug: a browser may probe
    /// the loopback redirect target (a speculative preconnect, a `/favicon.ico`
    /// fetch) BEFORE following the `302` to it. The callback catcher must
    /// answer-and-ignore such a probe and keep waiting, if it treated the
    /// probe as the callback and returned, the REAL redirect would land on a
    /// closed listener and the user would see "can't connect". Here the
    /// browser-thread fires a `/favicon.ico` probe first, then the real
    /// callback; login must still succeed.
    #[test]
    fn login_tolerates_a_probe_before_the_callback() {
        let server = start_server();
        let server_url = server.url.clone();

        let handle_slot: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> =
            Arc::new(Mutex::new(None));
        let slot = Arc::clone(&handle_slot);

        let tokens = login_with_opener(&server_url, move |open_url| {
            let open_url = open_url.to_string();
            let handle = std::thread::spawn(move || {
                let browser = reqwest::blocking::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(5))
                    .connect_timeout(std::time::Duration::from_secs(5))
                    .build()
                    .expect("build a non-redirecting blocking client");

                let auth_endpoint = open_url
                    .split_once('?')
                    .map_or(open_url.as_str(), |(base, _)| base)
                    .to_string();
                let redirect_uri = query_param(&open_url, "redirect_uri")
                    .expect("authorize URL should carry redirect_uri");
                let challenge = query_param(&open_url, "code_challenge")
                    .expect("authorize URL should carry code_challenge");
                let state =
                    query_param(&open_url, "state").expect("authorize URL should carry state");

                // (1) Probe the loopback FIRST with a non-callback request (no
                // `code`). The catcher must 404 it and keep listening.
                let probe = redirect_uri
                    .rsplit_once('/')
                    .map(|(base, _)| format!("{base}/favicon.ico"))
                    .expect("redirect_uri has a path to strip");
                let probe_resp = browser
                    .get(&probe)
                    .send()
                    .expect("probing the loopback should get a response, not hang");
                assert_eq!(
                    probe_resp.status(),
                    reqwest::StatusCode::NOT_FOUND,
                    "a non-callback probe must be answered-and-ignored (404), not consumed as the callback"
                );

                // (2) Now the real authorize → code → loopback callback.
                let post_resp = browser
                    .post(&auth_endpoint)
                    .form(&[
                        ("response_type", "code"),
                        ("client_id", "soul-player"),
                        ("redirect_uri", redirect_uri.as_str()),
                        ("code_challenge", challenge.as_str()),
                        ("code_challenge_method", "S256"),
                        ("state", state.as_str()),
                        ("scope", "library"),
                        ("username", "u"),
                        ("password", "p"),
                    ])
                    .send()
                    .expect("POST /authorize should succeed");
                assert_eq!(post_resp.status(), reqwest::StatusCode::FOUND);
                let location = post_resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .expect("302 should carry a Location header")
                    .to_str()
                    .expect("Location header should be valid UTF-8")
                    .to_string();
                browser_get_loopback(&browser, &location);
            });
            *slot.lock().expect("lock the browser-thread handle slot") = Some(handle);
            Ok(())
        })
        .expect("login must still succeed despite the pre-callback probe");

        handle_slot
            .lock()
            .expect("lock the browser-thread handle slot")
            .take()
            .expect("opener should have stored a browser-thread handle")
            .join()
            .expect("browser thread should not panic");

        assert!(
            !tokens.access.is_empty(),
            "login should yield an access token"
        );
        assert!(
            !tokens.refresh.is_empty(),
            "login should yield a refresh token"
        );
    }

    /// The CSRF guard, exercised through the same real loopback path as
    /// [`login_full_flow_catches_loopback_redirect`]: the browser-thread
    /// hits `login_with_opener`'s OWN loopback listener directly with a
    /// `state` that does NOT match what `login_with_opener` generated,
    /// this must be rejected before ever reaching [`exchange_code`], so no
    /// AS round-trip (GET/POST `/authorize`) is needed here at all.
    #[test]
    fn login_rejects_mismatched_state() {
        let server = start_server();

        let handle_slot: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> =
            Arc::new(Mutex::new(None));
        let slot = Arc::clone(&handle_slot);

        let result = login_with_opener(&server.url, move |open_url| {
            let open_url = open_url.to_string();
            let handle = std::thread::spawn(move || {
                let redirect_uri = query_param(&open_url, "redirect_uri")
                    .expect("authorize URL should carry redirect_uri");
                let loopback = format!("{redirect_uri}?code=bogus-code&state=totally-wrong-state");

                let browser = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(5))
                    .connect_timeout(std::time::Duration::from_secs(5))
                    .build()
                    .expect("build a blocking client");

                browser_get_loopback(&browser, &loopback);
            });
            *slot.lock().expect("lock the browser-thread handle slot") = Some(handle);
            Ok(())
        });

        handle_slot
            .lock()
            .expect("lock the browser-thread handle slot")
            .take()
            .expect("opener should have stored a browser-thread handle")
            .join()
            .expect("browser thread should not panic");

        assert!(
            result.is_err(),
            "a mismatched state must abort login as a possible CSRF, never return Tokens"
        );
    }

    /// Round-trips a refresh token through the REAL OS keychain backend
    /// (Windows Credential Manager, via the `windows-native` `keyring`
    /// feature), every other [`TokenStore`] test here uses [`MemStore`],
    /// which never touches the OS at all. `#[ignore]`d because it mutates
    /// real machine state; run explicitly with `--ignored`.
    ///
    /// Cleans up via a `Drop` guard so a failed assertion can never leave a
    /// stray credential behind in the user's Credential Manager.
    #[test]
    #[ignore = "hits the real OS keychain"]
    fn keyring_store_real_round_trip() {
        let server_url = format!("soul-player-test://ci-roundtrip-{}", std::process::id());
        let store = KeyringStore;

        struct DeleteGuard<'a> {
            store: &'a KeyringStore,
            server_url: &'a str,
        }
        impl Drop for DeleteGuard<'_> {
            fn drop(&mut self) {
                let _ = self.store.delete_refresh(self.server_url);
            }
        }
        let _guard = DeleteGuard {
            store: &store,
            server_url: &server_url,
        };

        store
            .save_refresh(&server_url, "real-keychain-refresh-token")
            .expect("save_refresh should write to the real OS keychain");
        assert_eq!(
            store.load_refresh(&server_url),
            Some("real-keychain-refresh-token".to_string()),
            "load_refresh should read back what save_refresh wrote from the real OS keychain"
        );

        store
            .delete_refresh(&server_url)
            .expect("delete_refresh should succeed against the real OS keychain");
        assert_eq!(
            store.load_refresh(&server_url),
            None,
            "load_refresh after delete_refresh should be None"
        );
    }
}
