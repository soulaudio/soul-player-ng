//! Acceptance E2E: add a remote source, sync its `.soulidx`, browse the
//! aggregated library, and play a real remote track end to end — against a
//! REAL in-process `soul-server`, not a faked HTTP boundary.
//!
//! This is the "add source and syncing .soulidx working, showing the albums
//! etc and being able to play tracks" deliverable, proven as one flow:
//! 1. stand up a real server over a small library of tagged fixtures,
//! 2. log in through the REAL standards-based OAuth 2.1 Authorization-Code
//!    +PKCE flow (simulating the system browser with a non-redirecting
//!    `reqwest` client, the same way `soul_player_host::remote`'s own
//!    `oauth_code_exchange_and_refresh` test does) and
//!    `PlayerHandle::add_remote_source_with_tokens` (the public API a head
//!    calls once it holds a `Tokens` pair),
//! 3. assert the aggregate `views()` reflects the synced catalog,
//! 4. resolve a remote track id through the same client-side resolution
//!    (`soul_player_host::remote`) and decode real audio from the
//!    downloaded bytes.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use soul_audio_core::Decoder;
use soul_player_backend::library::TrackSource;
use soul_player_host::PlayerHandle;
use soul_player_host::remote::{DownloadCache, MediaFetch, RemoteLibrary, RemoteServer, Tokens};
use soul_server::auth::signer::Signer;
use soul_server::domain::ports::UserStore;
use soul_server::domain::service::Services;
use soul_server::index::CoreIndex;
use soul_server::media::FsMedia;
use soul_server::proto::native::{self, AppState};
use soul_server::store::FileUsers;

/// Path to the repo's committed `testdata/` fixtures, three levels up from
/// this crate's manifest dir to the repo root (same convention as
/// `soul-player-host::remote`'s own tests).
fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// A real `soul-server` native router, bound to an ephemeral localhost port
/// and seeded with one user over a server-library temp dir containing
/// `fixtures` (copied out of `testdata/`). Mirrors
/// `soul_player_host::remote`'s own `start_server_over` test helper.
struct TestServer {
    url: String,
    /// Kept alive so the server's worker threads (and listening socket) keep
    /// running for the test's duration; never read directly.
    _runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

fn start_server(fixtures: &[&str]) -> TestServer {
    let dir = tempfile::tempdir().expect("create server temp dir");
    let library_root = dir.path().join("library");
    fs::create_dir_all(&library_root).expect("create server library dir");
    for name in fixtures {
        fs::copy(testdata_dir().join(name), library_root.join(name))
            .unwrap_or_else(|e| panic!("copy fixture {name}: {e}"));
    }

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
    // to a server that isn't there. Mirrors `soul_player_host::remote`'s own
    // `start_server_over` test helper.
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
// Minimal, local copies of the browser-side-of-the-flow helpers
// `soul_player_host::remote`'s own `oauth_code_exchange_and_refresh` test
// uses — those are private to that module, and this file is a separate
// integration-test crate, so the PKCE/state/query-param plumbing is
// reproduced here rather than exposed just for a test.
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
/// through unchanged (only ever reads values this test or the trusted
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
/// for a real [`Tokens`] pair — the standards-based replacement for the
/// `Signer`-minted-token shortcut this test used to take.
fn oauth_login(server_url: &str, username: &str, password: &str) -> Tokens {
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

#[test]
fn remote_add_sync_browse_play() {
    // Three real, differently-containered fixtures with readable tags — the
    // server's whole "library".
    const FIXTURES: &[&str] = &["meta.aiff", "meta.caf", "meta-id3.aac"];
    let server = start_server(FIXTURES);

    // --- 1/2. Log in via the REAL browser-facing OAuth 2.1 flow, then add +
    //        sync through the public API a head actually calls once it
    //        holds a `Tokens` pair. (The interactive `add_remote_source`
    //        itself — opening the system browser + the loopback catcher —
    //        is E4's job to wire into a head and exercise interactively;
    //        this E2E test drives the standards login headlessly the same
    //        way `soul_player_host::remote`'s own
    //        `oauth_code_exchange_and_refresh` test does, then hands the
    //        resulting tokens straight to `add_remote_source_with_tokens`.)
    let handle = PlayerHandle::offline();
    let tokens = oauth_login(&server.url, "e2e", "pw");
    let access_token = tokens.access.clone();
    handle
        .add_remote_source_with_tokens(&server.url, tokens)
        .expect("add remote source: fetch the index, fold it into the library");

    // --- 3. Browse: the synced `.soulidx` populated the aggregate library. ---
    let views = handle.views();
    assert_eq!(
        views.len(),
        FIXTURES.len(),
        "expected one browsable view per fixture synced from the server, got {views:?}"
    );
    // Every fixture here is the same reference piece (Kimiko Ishizaka's "Aria"
    // from the Open Goldberg Variations, muxed into several containers) — a
    // known, stable title every synced view should carry, proving the real
    // `.soulidx` tags round-tripped through the wire protocol, not just a
    // track count.
    assert!(
        views.iter().all(|v| v.title.as_deref() == Some("Aria")),
        "expected every synced view titled \"Aria\", got {views:?}"
    );
    assert!(
        views
            .iter()
            .any(|v| v.album.as_deref() == Some("Open Goldberg Variations")),
        "expected at least one synced view to carry its album tag, got {views:?}"
    );

    // --- 4. Play: resolve a remote track id through the SAME client-side
    //        resolution `add_remote_source_with_tokens` uses (fetch_index ->
    //        RemoteLibrary), then decode real audio via `open_decoder` — the
    //        STREAMING path (HTTP Range + `open_seekable_src`), not a
    //        download-then-play. A separate `DownloadCache` (own temp dir) so
    //        this doesn't share the host's real on-disk cache; it's only
    //        there as `open_decoder`'s fallback, and the assertion below
    //        proves the happy streaming path never touches it.
    let remote_server = Arc::new(RemoteServer {
        url: server.url.clone(),
        access_token,
    });
    let blobs = soul_player_host::remote::fetch_index(&remote_server).expect("fetch_index");
    let cache_dir = tempfile::tempdir().expect("create download cache dir");
    let fetch: Arc<dyn MediaFetch> = Arc::new(DownloadCache {
        dir: cache_dir.path().to_path_buf(),
    });
    // No local sources (the handle was never pointed at a local folder), so
    // this rebuilds the exact aggregate `add_remote_source` swapped in: one
    // remote block at source 0 — the same ids `views()` above returned.
    let remote_lib = Arc::new(RemoteLibrary::build(
        Vec::new(),
        vec![(remote_server, blobs)],
        fetch,
    ));

    let id = views[0].id;

    // `OwnedDecoder` is large by value (the boxed `AnyDecoder`/Vorbis
    // variants) — a big-stack thread, same precaution
    // `soul_audio_desktop::decoder`'s own `open_track` tests take.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut decoder = remote_lib
                .open_decoder(id)
                .expect("the remote track should open a STREAMING decoder");
            let mut buf = [0i32; 4096];
            let samples = decoder
                .decode_into(&mut buf)
                .expect("decoding the first block of the remote track should succeed");
            assert!(
                samples > 0,
                "expected a non-empty decoded block from the remote track"
            );
        })
        .expect("spawn decode thread")
        .join()
        .expect("decode thread panicked");

    // The happy streaming path never falls back to downloading the whole
    // file first — the cache directory should still be completely empty.
    let cache_is_empty = fs::read_dir(cache_dir.path())
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(true);
    assert!(
        cache_is_empty,
        "playing the remote track via open_decoder must not have fallen back \
         to the download cache"
    );
}

/// `set_remote_enabled` toggles a connected remote off and back on, mirroring
/// the local-folder enable/disable toggle: the remote stays retained (it's
/// still in `sources()`, never disconnected, no network/keychain touched) but
/// while disabled its tracks drop out of `views()` and its `SourceStatus`
/// reports `enabled: false` with `track_count: 0`; re-enabling restores both
/// from the already-fetched blobs, with no re-fetch needed.
#[test]
fn remote_disable_drops_its_tracks_and_reenable_restores_them() {
    const FIXTURES: &[&str] = &["meta.aiff", "meta.caf", "meta-id3.aac"];
    let server = start_server(FIXTURES);

    let handle = PlayerHandle::offline();
    let tokens = oauth_login(&server.url, "e2e", "pw");
    handle
        .add_remote_source_with_tokens(&server.url, tokens)
        .expect("add remote source: fetch the index, fold it into the library");

    assert_eq!(
        handle.views().len(),
        FIXTURES.len(),
        "sanity: the remote's tracks are in the library before any toggle"
    );
    let status = handle
        .sources()
        .into_iter()
        .find(|s| matches!(&s.kind, soul_player_host::SourceKind::Remote { url } if url == &server.url))
        .expect("the connected remote has a SourceStatus row");
    assert!(status.enabled, "a freshly-added remote starts enabled");
    assert_eq!(status.track_count, FIXTURES.len());

    // Disable: the remote stays configured, but its tracks leave the library.
    handle
        .set_remote_enabled(&server.url, false)
        .expect("set_remote_enabled(false) should succeed");
    assert!(
        handle.views().is_empty(),
        "a disabled remote's tracks must leave views()"
    );
    let disabled = handle
        .sources()
        .into_iter()
        .find(|s| matches!(&s.kind, soul_player_host::SourceKind::Remote { url } if url == &server.url))
        .expect("the remote still lists in sources() while disabled");
    assert!(
        !disabled.enabled,
        "sources() should reflect the disabled flag"
    );
    assert_eq!(
        disabled.track_count, 0,
        "a disabled remote reports 0 tracks, like a disabled local folder"
    );

    // Re-enable: the already-fetched blobs restore the tracks, no re-fetch.
    handle
        .set_remote_enabled(&server.url, true)
        .expect("set_remote_enabled(true) should succeed");
    assert_eq!(
        handle.views().len(),
        FIXTURES.len(),
        "re-enabling restores the remote's tracks to views()"
    );
    let reenabled = handle
        .sources()
        .into_iter()
        .find(|s| matches!(&s.kind, soul_player_host::SourceKind::Remote { url } if url == &server.url))
        .expect("the remote still lists in sources() once re-enabled");
    assert!(reenabled.enabled);
    assert_eq!(reenabled.track_count, FIXTURES.len());
}

/// Robustness: a remote that was online when connected can go OFFLINE later
/// (the server itself goes away) without being dropped from `sources()` or
/// losing its already-synced track count — only the health probe's `online`
/// flag (and the `available` mirror) changes. Proven against a REAL server
/// across two real `PlayerHandle::refresh_remote_health` probes, the second
/// one taken after the server's listener has actually closed — the UI-facing
/// counterpart of the title-bar health indicator flipping from healthy to
/// down as a remote drops out from under a live session, and the "never wipe
/// the library over a transient outage" rule `SourceStatus`'s own docs state
/// for a local root, now proven for a remote too.
#[test]
fn remote_health_flips_offline_after_the_server_goes_away_but_track_count_survives() {
    const FIXTURES: &[&str] = &["meta.aiff", "meta.caf", "meta-id3.aac"];
    let server = start_server(FIXTURES);
    let url = server.url.clone();

    let handle = PlayerHandle::offline();
    let tokens = oauth_login(&url, "e2e", "pw");
    handle
        .add_remote_source_with_tokens(&url, tokens)
        .expect("add remote source: fetch the index, fold it into the library");

    let online_status = handle
        .refresh_remote_health()
        .into_iter()
        .find(|s| matches!(&s.kind, soul_player_host::SourceKind::Remote { url: u } if u == &url))
        .expect("the remote has a status row while its server is up");
    assert!(
        online_status
            .remote
            .as_ref()
            .expect("a remote's health is always Some")
            .online,
        "sanity: the probe reads online while the server is reachable"
    );
    assert_eq!(online_status.track_count, FIXTURES.len());

    // The server goes away entirely: dropping it tears down its tokio runtime
    // and closes its listener, so the next probe's connection attempt fails
    // outright rather than timing out.
    drop(server);

    let offline_status = handle
        .refresh_remote_health()
        .into_iter()
        .find(|s| matches!(&s.kind, soul_player_host::SourceKind::Remote { url: u } if u == &url))
        .expect("the remote STAYS listed once its server is gone");
    let health = offline_status
        .remote
        .as_ref()
        .expect("a remote's health is always Some, even offline");
    assert!(
        !health.online,
        "the probe reports offline once the server is actually gone"
    );
    assert!(
        !offline_status.available,
        "available mirrors the health probe for a remote"
    );
    assert_eq!(
        offline_status.track_count,
        FIXTURES.len(),
        "the already-synced track count must survive a health-only outage"
    );
    assert!(
        offline_status.enabled,
        "going offline must not touch the enabled flag"
    );
}

/// Dedup: calling `add_remote_source_with_tokens` twice for the SAME url
/// (e.g. a reconnect, or the user re-running the connect flow) must REPLACE
/// the existing `Inner::remotes` entry, never append a second one — pins the
/// `retain` fix in `add_remote_source_with_tokens`. Proven with a fresh
/// `Tokens` pair each time (a real second OAuth login), the way a real
/// reconnect would present tokens.
#[test]
fn remote_add_twice_same_url_replaces_not_duplicates() {
    const FIXTURES: &[&str] = &["meta.aiff", "meta.caf", "meta-id3.aac"];
    let server = start_server(FIXTURES);

    let handle = PlayerHandle::offline();

    let first_tokens = oauth_login(&server.url, "e2e", "pw");
    handle
        .add_remote_source_with_tokens(&server.url, first_tokens)
        .expect("first add: fetch the index, fold it into the library");

    let second_tokens = oauth_login(&server.url, "e2e", "pw");
    handle
        .add_remote_source_with_tokens(&server.url, second_tokens)
        .expect("second add of the SAME url: should replace, not duplicate");

    let remotes: Vec<_> = handle
        .sources()
        .into_iter()
        .filter(|s| matches!(&s.kind, soul_player_host::SourceKind::Remote { url } if url == &server.url))
        .collect();
    assert_eq!(
        remotes.len(),
        1,
        "re-adding the same url must leave exactly one Remote source, got {remotes:?}"
    );

    let views = handle.views();
    assert_eq!(
        views.len(),
        FIXTURES.len(),
        "re-adding the same url must not double the aggregated track count, got {views:?}"
    );
}

/// Normalization: a url re-added with a trailing slash (`"{url}/"`) must
/// match the same server's already-retained entry (added without one) — the
/// `trim_end_matches('/')` normalization `add_remote_source_with_tokens` and
/// `remove_remote_source`/`set_remote_enabled` share. Without it, the two
/// forms would be treated as different urls and the server would show up
/// twice.
#[test]
fn remote_add_with_trailing_slash_collapses_to_the_same_entry() {
    const FIXTURES: &[&str] = &["meta.aiff", "meta.caf", "meta-id3.aac"];
    let server = start_server(FIXTURES);

    let handle = PlayerHandle::offline();

    let tokens = oauth_login(&server.url, "e2e", "pw");
    handle
        .add_remote_source_with_tokens(&server.url, tokens)
        .expect("first add (no trailing slash): fetch the index, fold it into the library");

    let trailing_slash_url = format!("{}/", server.url);
    let tokens_again = oauth_login(&server.url, "e2e", "pw");
    handle
        .add_remote_source_with_tokens(&trailing_slash_url, tokens_again)
        .expect("second add (trailing slash): should normalize onto the same entry");

    let remotes: Vec<_> = handle
        .sources()
        .into_iter()
        .filter(|s| matches!(&s.kind, soul_player_host::SourceKind::Remote { url } if url == &server.url))
        .collect();
    assert_eq!(
        remotes.len(),
        1,
        "a trailing-slash re-add must normalize onto the same Remote entry, got {remotes:?}"
    );

    let views = handle.views();
    assert_eq!(
        views.len(),
        FIXTURES.len(),
        "a trailing-slash re-add must not double the aggregated track count, got {views:?}"
    );
}

/// Regression (the "click Rescan and the remote shows 0 tracks" bug): the
/// Sources "Rescan" button reconciles the LOCAL folder set only (the remote
/// isn't a watched folder — `settings.rs`'s `rescan_button` filters to
/// `SourceKind::Local` before calling `set_folders`), but the shared
/// `rescan` tail it runs through swaps in a fresh local `Library` via
/// `set_library`, whose own doc comment notes it clears
/// `Inner::remote_library` because a fresh library's compound ids don't
/// match the old aggregate. Before the fix, nothing rebuilt that aggregate
/// afterward: the remote's tracks silently vanished from the returned
/// `WatchUpdate.views` and `remote_statuses` (reading the now-`None`
/// aggregate) reported the remote's `track_count` as 0, even though the
/// remote was still fully connected and healthy.
///
/// Pins the fix (`rescan_build` now calls `rebuild_remote_library` — the
/// same tail `add_remote_source` uses — before computing `views`): a rescan
/// over a local root, with a remote already connected, must keep the
/// remote's tracks in `views` (each still stamped with its
/// `remote_source`), report the remote's REAL non-zero `track_count`, carry
/// its live health, and list it exactly once.
#[test]
fn rescan_keeps_a_connected_remotes_tracks_and_live_health() {
    const FIXTURES: &[&str] = &["meta.aiff", "meta.caf", "meta-id3.aac"];
    let server = start_server(FIXTURES);

    let handle = PlayerHandle::offline();
    let tokens = oauth_login(&server.url, "e2e", "pw");
    handle
        .add_remote_source_with_tokens(&server.url, tokens)
        .expect("add remote source: fetch the index, fold it into the library");
    assert_eq!(
        handle.views().len(),
        FIXTURES.len(),
        "sanity: the remote's tracks are in the library before any rescan"
    );

    // A local root, deliberately empty — the bug reproduces with or without
    // local tracks of its own; this isolates the assertion to whether the
    // REMOTE survives the reconcile.
    let local_root = tempfile::tempdir().expect("create local root");

    // Mirrors the Rescan button: ONLY local roots are passed to
    // `set_folders` (the remote's synthetic path is excluded — commit
    // b2fa8da), so this is exactly the "click Rescan" path.
    let update = handle
        .set_folders(&[local_root.path().to_path_buf()])
        .expect("set_folders (a rescan) must not drop the connected remote");

    let remotes: Vec<_> = update
        .sources
        .iter()
        .filter(|s| matches!(&s.kind, soul_player_host::SourceKind::Remote { url } if url == &server.url))
        .collect();
    assert_eq!(
        remotes.len(),
        1,
        "exactly one Remote entry after a rescan, no duplicate; got {remotes:?}"
    );
    let remote_status = remotes[0];
    assert!(
        remote_status.remote.is_some(),
        "the Remote entry must carry its health, not go back to None"
    );
    assert_eq!(
        remote_status.track_count,
        FIXTURES.len(),
        "a rescan must not zero out the remote's track_count, got {remote_status:?}"
    );

    // The aggregated views must still carry the remote's tracks, each
    // stamped with its remote_source — the "not local-only after a rescan"
    // half of the bug.
    let remote_views_in_update = update
        .views
        .iter()
        .filter(|v| v.remote_source.is_some())
        .count();
    assert_eq!(
        remote_views_in_update,
        FIXTURES.len(),
        "the remote's tracks must remain in WatchUpdate.views after a rescan, got {:?}",
        update.views
    );

    // The handle's own cached views (not just the just-returned WatchUpdate)
    // must agree — the whole point of restoring the aggregate is that every
    // reader of the live library, not just this one return value, sees it.
    assert_eq!(
        handle.views().len(),
        FIXTURES.len(),
        "handle.views() must still include the remote's tracks after the rescan"
    );
}
