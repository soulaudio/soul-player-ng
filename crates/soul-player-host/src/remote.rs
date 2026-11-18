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
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use soul_audio_multicatalog::{pack, unpack};
use soul_player_backend::library::{Library, TrackSource};

use crate::RemoteHealth;

/// A soul-server the player has logged into via OAuth: its base URL (no
/// trailing slash) and the short-lived JWT access token obtained via
/// [`login`] (or [`exchange_code`]/[`refresh`]) and carried OPAQUELY — this
/// client never parses or validates it, it only ever forwards it as a
/// bearer token.
#[derive(Clone, Debug)]
pub struct RemoteServer {
    pub url: String,
    pub access_token: String,
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
    /// This server's own cache subdirectory, named by a stable hash of its URL.
    /// Track ids are per-server (compound catalog ids), so two different servers
    /// routinely emit the same low id — keeping each server's downloads in its own
    /// namespace prevents serving one server's bytes for another's track.
    fn server_dir(&self, server: &RemoteServer) -> PathBuf {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        server.url.hash(&mut hasher);
        self.dir.join(format!("{:016x}", hasher.finish()))
    }
    /// The cache file path for `server`'s `track_id` — stable and extension-less,
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
        let response = reqwest::blocking::Client::new()
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

/// Where the desktop client persists the OAuth **refresh token** obtained at
/// login, keyed by the remote server's base URL (so logging into several
/// servers keeps independent tokens). A trait so production code can depend
/// on [`KeyringStore`] (the OS keychain) while tests swap in [`MemStore`]
/// without touching the real Credential Manager/Keychain/Secret Service.
///
/// The OAuth **access** token is short-lived and never persisted here — only
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
            // is not an error — the caller just wants it gone.
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
pub fn fetch_index(server: &RemoteServer) -> anyhow::Result<Vec<Vec<u8>>> {
    let endpoint = format!("{}/v0/index", server.url);
    let response = reqwest::blocking::Client::new()
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

/// Probe `server`'s health with one unauthenticated `GET /v0/ping`, timing
/// the round-trip. Offline is a VALUE, not an error: this never panics and
/// never returns `Err` — any failure (connect refused, timeout, a non-2xx
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
    // Measured right after the response headers arrive — the round-trip —
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
/// `/v0/*` call (held OPAQUELY — this client never parses or validates it,
/// it only ever forwards it); `refresh` is the long-lived, rotating token
/// [`TokenStore`] persists so a later session can skip the interactive
/// browser login via [`refresh`].
pub struct Tokens {
    pub access: String,
    pub refresh: String,
}

/// Fetch `{url}/.well-known/openid-configuration` and pull out the two
/// endpoints this client needs: where to send the user's browser
/// (`authorization_endpoint`) and where to redeem a code/refresh token
/// (`token_endpoint`).
fn discover(url: &str) -> anyhow::Result<(String, String)> {
    let endpoint = format!("{url}/.well-known/openid-configuration");
    let response = reqwest::blocking::Client::new()
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

/// `n` CSPRNG bytes via `getrandom` (the OS random source — no `rand` crate
/// needed for this).
fn random_bytes(n: usize) -> anyhow::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    // `getrandom::Error` doesn't implement `std::error::Error` under this
    // crate's default (no `std` feature) build, so `anyhow::Context` isn't
    // available on it — fold it into an anyhow error via `Display` instead.
    getrandom::getrandom(&mut buf)
        .map_err(|e| anyhow::anyhow!("reading CSPRNG bytes for OAuth/PKCE: {e}"))?;
    Ok(buf)
}

/// A fresh PKCE verifier/challenge pair (RFC 7636 §4.1/§4.2): `verifier` is
/// base64url-no-pad of 32 random bytes; `challenge` is
/// base64url-no-pad(SHA256(verifier)) (the `S256` method — the only one this
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
/// Mirrors the server's own `percent_encode` — written by hand here too
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
/// rather than erroring — this only ever reads values this client or the
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
/// [`Tokens`]. Shared by [`exchange_code`] and [`refresh`] — the only
/// difference between the two grants is which form fields they send.
fn post_token(token_endpoint: &str, form: &[(&str, &str)]) -> anyhow::Result<Tokens> {
    let response = reqwest::blocking::Client::new()
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
/// before giving up — generous (a password manager, an SSO redirect, a
/// distracted human) but finite, so [`login`] can never hang forever.
const CALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Accepts exactly one connection on `listener` (polling with a timeout
/// rather than blocking forever, so a browser that never shows up can't hang
/// the caller), reads its HTTP request line, and pulls `code`/`state` out of
/// the `GET /callback?code=..&state=..` request — then writes back a
/// minimal `200 OK` so the browser tab shows a friendly confirmation instead
/// of hanging or erroring.
fn accept_callback(listener: &std::net::TcpListener) -> anyhow::Result<(String, String)> {
    use std::io::BufRead as _;

    listener
        .set_nonblocking(true)
        .context("setting the loopback listener non-blocking")?;
    let deadline = std::time::Instant::now() + CALLBACK_TIMEOUT;

    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _addr)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    bail!("timed out waiting for the browser login to complete");
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e).context("accepting the OAuth callback connection"),
        }
    };
    stream
        .set_nonblocking(false)
        .context("setting the accepted OAuth callback connection blocking")?;

    let (code, state) = {
        let mut reader = std::io::BufReader::new(&stream);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .context("reading the OAuth callback request line")?;
        // "GET /callback?code=..&state=.. HTTP/1.1"
        let path = request_line
            .split_whitespace()
            .nth(1)
            .context("malformed OAuth callback request line")?;
        let code = query_param(path, "code").context("OAuth callback had no `code`")?;
        let state = query_param(path, "state").context("OAuth callback had no `state`")?;
        (code, state)
    };

    let body = "You can close this tab and return to soul-player.";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream
        .write_all(response.as_bytes())
        .context("writing the OAuth callback response")?;

    Ok((code, state))
}

/// The full interactive login: discover the AS, bind a LOOPBACK-only
/// listener (never `0.0.0.0` — RFC 8252's native-app redirect is only ever
/// safe to accept from the local machine), build the PKCE+state
/// `/authorize` URL, open it in the system browser, accept the single
/// resulting callback, verify `state` (a CSRF guard — see RFC 6749 §10.12),
/// and exchange the code for [`Tokens`].
///
/// A thin wrapper over [`login_with_opener`] that opens the URL in the
/// user's actual system browser — see that function for the real logic and
/// for how tests exercise it without a browser.
pub fn login(url: &str) -> anyhow::Result<Tokens> {
    login_with_opener(url, |u| open::that(u).map_err(anyhow::Error::from))
}

/// [`login`]'s real logic, with how the `/authorize` URL gets opened
/// factored out behind `open_fn` so tests can stand in for the system
/// browser: a test's `open_fn` typically spawns a thread that plays browser
/// (GET the login form, POST credentials, follow the redirect back to this
/// function's OWN loopback listener) and returns immediately, exactly like
/// [`open::that`] does — `login_with_opener` can't call `open_fn` and THEN
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
        bail!("OAuth callback state mismatch — possible CSRF, aborting login");
    }

    exchange_code(url, &code, &verifier, &redirect_uri)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

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
        /// user directly off its `Signer` — the cheap stand-in for a full
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
    /// `testdata/` tree — so a test can stand up a server over a DIFFERENT
    /// subset of fixtures than whatever it scanned locally.
    ///
    /// Binds the ephemeral port SYNCHRONOUSLY, up front, rather than inside
    /// the spawned async task: the OAuth discovery document advertises
    /// `authorization_endpoint`/`token_endpoint` built from `issuer`, so
    /// `issuer` must be this server's actual bound `http://127.0.0.1:PORT`
    /// base — not a fixed placeholder — or a client that resolves endpoints
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

    /// Extract the value of `key` from `location`'s query string (used here
    /// to pull `code`/`state` back out of the server's `302 Location`
    /// header — the same parsing [`query_param`] does for the real loopback
    /// callback in [`login`]).
    fn location_param(location: &str, key: &str) -> Option<String> {
        query_param(location, key)
    }

    /// GETs `url` (the loopback callback) with a few short retries rather
    /// than one shot — tolerating the brief window between
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
    /// `"u"`/`"p"` user, returning the resulting [`Tokens`] — the "get Tokens
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
    /// carries straight into `fetch_index` — no browser, no OS keychain —
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
    /// no latency or version — never an `Err`/panic, offline is a value.
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

        assert!(!health.online, "nothing is listening — must report offline");
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
    /// [`crate::TrackView::remote`] from the aggregate's compound id, using
    /// the SAME sentinel-root convention `sources_lists_remote_with_kind_and_health`
    /// already proved for `track_count` — a remote server's tracks come back
    /// `remote == true`, the configured local folder's stay `remote == false`.
    #[test]
    fn views_marks_remote_tracks_remote_and_local_tracks_not_remote() {
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

        assert!(
            views.iter().any(|v| v.remote),
            "the synced remote catalog should contribute at least one track marked remote"
        );
        assert!(
            views.iter().any(|v| !v.remote),
            "the configured local folder should still contribute at least one non-remote track"
        );
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

    /// Drives [`login_with_opener`] through its REAL bind→accept→parse→
    /// state-verify→exchange path — the one `login` itself has but no
    /// prior test exercised, since [`oauth_code_exchange_and_refresh`]
    /// calls [`exchange_code`] directly and never touches a loopback
    /// socket at all.
    ///
    /// The `open_fn` here stands in for `open::that`: it spawns a thread
    /// (so this call returns immediately, exactly like the real browser
    /// launcher does) that plays the browser — GET the login form POSTed
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
        // than joined inline — joined below, AFTER `login_with_opener`
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
                // TcpListener accepts — the real loopback redirect catch.
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

    /// The CSRF guard, exercised through the same real loopback path as
    /// [`login_full_flow_catches_loopback_redirect`]: the browser-thread
    /// hits `login_with_opener`'s OWN loopback listener directly with a
    /// `state` that does NOT match what `login_with_opener` generated —
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
    /// feature) — every other [`TokenStore`] test here uses [`MemStore`],
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
