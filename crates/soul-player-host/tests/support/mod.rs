//! Shared harness for the host's acceptance E2E tests: a REAL in-process
//! `soul-server` over a temp library, plus the browser-side half of the real
//! OAuth 2.1 Authorization-Code+PKCE login.
//!
//! Both `remote_e2e.rs` (the add-source/sync/browse/play flow) and
//! `format_matrix_e2e.rs` (every supported container, local *and* remote) stand
//! up the same thing, so it lives here once instead of being copy-pasted per
//! test binary. Nothing here fakes the HTTP boundary — the server is the real
//! `axum` router on a real loopback socket, and the login is the real
//! standards-based flow driven through a non-redirecting `reqwest` client.
//!
//! Each integration-test file is its own crate, so this module is compiled
//! separately into every test binary that declares it — and any helper that
//! binary doesn't happen to call reads as dead code there. Allow it module-wide
//! rather than making each test use every constructor just to stay quiet
//! (CI runs `clippy -D warnings`).
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use soul_player_host::remote::Tokens;
use soul_server::auth::signer::Signer;
use soul_server::domain::ports::UserStore;
use soul_server::domain::service::Services;
use soul_server::index::CoreIndex;
use soul_server::media::FsMedia;
use soul_server::proto::native::{self, AppState};
use soul_server::store::FileUsers;

/// Path to the repo's committed `testdata/` fixtures, three levels up from
/// this crate's manifest dir to the repo root (same convention as
/// `soul_player_host::remote`'s own tests).
pub fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// A real `soul-server` native router, bound to an ephemeral localhost port
/// and seeded with one user over a server-library temp dir.
pub struct TestServer {
    pub url: String,
    /// Kept alive so the server's worker threads (and listening socket) keep
    /// running for the test's duration; never read directly.
    _runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

/// Stand a server up over a library dir containing `fixtures`, each copied out
/// of `testdata/` under the name it should carry on the server
/// (`(source_fixture, name_in_the_library)`).
///
/// The rename matters for the format matrix: a fixture committed as
/// `meta-alac.m4a` has to be served as `aria.m4a` for the server's scan — and
/// then the client's — to exercise the `.m4a` extension path.
pub fn start_server_with_names(fixtures: &[(&str, &str)]) -> TestServer {
    let dir = tempfile::tempdir().expect("create server temp dir");
    let library_root = dir.path().join("library");
    fs::create_dir_all(&library_root).expect("create server library dir");
    for (src, dest) in fixtures {
        fs::copy(testdata_dir().join(src), library_root.join(dest))
            .unwrap_or_else(|e| panic!("copy fixture {src} -> {dest}: {e}"));
    }
    start_server_over_root(dir, library_root)
}

/// `start_server_with_names` for the common case where each fixture keeps its
/// committed name.
pub fn start_server(fixtures: &[&str]) -> TestServer {
    let pairs: Vec<(&str, &str)> = fixtures.iter().map(|f| (*f, *f)).collect();
    start_server_with_names(&pairs)
}

/// Serve an ALREADY-POPULATED directory — used when a test needs the server and
/// a local source to read the very same bytes under the very same names.
pub fn start_server_serving(dir: &Path) -> TestServer {
    let holder = tempfile::tempdir().expect("create server temp dir");
    start_server_over_root(holder, dir.to_path_buf())
}

/// Shared tail of the constructors: scan `library_root`, seed a user, bind an
/// ephemeral port, and serve the native router on a background runtime.
/// `dir` is the temp dir whose lifetime the returned server must keep alive
/// (the user store / signer live there).
fn start_server_over_root(dir: tempfile::TempDir, library_root: PathBuf) -> TestServer {
    let index = CoreIndex::scan(vec![library_root]).expect("scan the server library");
    let users = FileUsers::open(&dir.path().join("users.json")).expect("open user store");
    users.add_user("e2e", "pw").expect("seed the e2e user");
    let services = Arc::new(Services {
        index,
        media: FsMedia,
        users,
    });

    // Bind the ephemeral port SYNCHRONOUSLY, up front — the OAuth discovery
    // document advertises `authorization_endpoint`/`token_endpoint` built
    // from `issuer`, so `issuer` must be this server's REAL bound
    // `http://127.0.0.1:PORT` base (not a portless placeholder), or
    // `exchange_code`'s discovery fetch would be handed URLs with no port,
    // to a server that isn't there.
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    std_listener
        .set_nonblocking(true)
        .expect("set the bound listener non-blocking for tokio");
    let addr = std_listener.local_addr().expect("bound socket has an addr");
    let issuer = format!("http://{addr}");

    let signer_dir = dir.path().join("signer");
    let signer = Arc::new(
        Signer::load_or_create(&signer_dir, issuer.clone()).expect("load_or_create signer"),
    );
    let state = AppState::new(services, signer, issuer, 3600);

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
        _runtime: runtime,
        _dir: dir,
    }
}

// ---------------------------------------------------------------------------
// The browser side of the real OAuth 2.1 Authorization-Code+PKCE flow.
// `soul_player_host::remote`'s own `oauth_code_exchange_and_refresh` test has
// equivalents, but they're private to that module and these are separate
// integration-test crates, so the PKCE/state/query-param plumbing lives here
// rather than being exposed from the library just for tests.
// ---------------------------------------------------------------------------

/// A fresh PKCE verifier/challenge pair (RFC 7636 §4.1/§4.2, `S256`):
/// `verifier` is base64url-no-pad of 32 random bytes; `challenge` is
/// base64url-no-pad(SHA256(verifier)).
fn pkce_pair() -> (String, String) {
    let mut verifier_bytes = [0u8; 32];
    getrandom::getrandom(&mut verifier_bytes).expect("read CSPRNG bytes for the PKCE verifier");
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// A fresh CSRF `state` value: base64url-no-pad of 16 random bytes.
fn random_state() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("read CSPRNG bytes for the OAuth state");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Percent-encodes `s` for safe inclusion as a query-string value (RFC 3986
/// unreserved set passes through unescaped; everything else becomes `%XX`).
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

/// Builds the `/authorize` URL the (simulated) browser is pointed at.
fn authorize_url(auth_endpoint: &str, challenge: &str, state: &str, redirect_uri: &str) -> String {
    format!(
        "{auth_endpoint}?response_type=code&client_id=soul-player&redirect_uri={}\
         &code_challenge={challenge}&code_challenge_method=S256&state={state}&scope=library",
        percent_encode(redirect_uri)
    )
}

/// Decodes `%XX` escapes in a query-string value; malformed escapes pass
/// through unchanged (only ever reads values these tests or the trusted
/// `soul-server` AS produced).
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

/// Extract `key`'s value out of the server's `302 Location` header (a full
/// callback URL carrying `code`/`state` in its query string).
fn location_param(location: &str, key: &str) -> Option<String> {
    let query = location.split_once('?').map_or(location, |(_, q)| q);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

/// Drives the real OAuth 2.1 Authorization-Code+PKCE login against
/// `server_url`, simulating the system browser with a non-redirecting
/// blocking `reqwest` client: `GET /authorize` for the login form,
/// `POST /authorize` with the seeded credentials to get the `302` carrying
/// the code, then [`soul_player_host::remote::exchange_code`] to redeem it
/// for a real [`Tokens`] pair.
pub fn oauth_login(server_url: &str, username: &str, password: &str) -> Tokens {
    let redirect_uri = "http://127.0.0.1:1/callback";
    let (verifier, challenge) = pkce_pair();
    let state = random_state();

    let browser = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("build a non-redirecting client");

    let auth_endpoint = format!("{server_url}/authorize");
    let get_url = authorize_url(&auth_endpoint, &challenge, &state, redirect_uri);
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
            ("redirect_uri", redirect_uri),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("scope", "library"),
            ("username", username),
            ("password", password),
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
    let returned_state = location_param(&location, "state").expect("Location should carry state");
    assert_eq!(returned_state, state, "state must round-trip unchanged");
    let code = location_param(&location, "code").expect("Location should carry a code");

    soul_player_host::remote::exchange_code(server_url, &code, &verifier, redirect_uri)
        .expect("exchange_code should succeed with a valid code+verifier")
}
