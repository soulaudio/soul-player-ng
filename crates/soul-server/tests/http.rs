//! HTTP-level tests for the native `/v0` protocol, exercised through
//! [`tower::ServiceExt::oneshot`] over a real `Services` (no socket bound):
//! a `CoreIndex` scanning the repo's `testdata/` fixtures, the real
//! `FsMedia`, and a temp-file `FileUsers` with one seeded user.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use soul_server::auth::signer::Signer;
use soul_server::domain::ports::{IndexPort, UserStore};
use soul_server::domain::service::Services;
use soul_server::index::CoreIndex;
use soul_server::media::FsMedia;
use soul_server::proto::native::{self, AppServices, AppState};
use soul_server::store::FileUsers;

/// Path to the committed `testdata/` fixtures, two levels up from this
/// crate's manifest dir to the repo root (same convention `index.rs`'s own
/// tests use).
fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

/// Build a router over a real `Services`: `CoreIndex` scanning `testdata/`,
/// `FsMedia`, and a fresh temp-file `FileUsers` with one seeded user
/// (`"u"` / `"p"`). Returns the router plus the `TempDir` guard (keep it
/// alive for the test's duration) and the `Services` handle for assertions
/// that need to look something up directly (e.g. `locate`).
fn test_router() -> (Router, tempfile::TempDir, Arc<AppServices>, Arc<Signer>) {
    let dir = tempfile::tempdir().unwrap();
    let index = CoreIndex::scan(vec![testdata_dir()]).expect("scan testdata/");
    let media = FsMedia;
    let users = FileUsers::open(&dir.path().join("users.json")).expect("open user store");
    users.add_user("u", "p").expect("seed test user");

    let services = Arc::new(Services {
        index,
        media,
        users,
    });
    let signer_dir = dir.path().join("signer");
    let signer = Arc::new(
        Signer::load_or_create(&signer_dir, "http://test.invalid".to_string())
            .expect("load_or_create signer"),
    );
    let state = AppState::new(
        services.clone(),
        signer.clone(),
        "http://test.invalid".to_string(),
        3600,
    );
    let router = native::router(state);
    (router, dir, services, signer)
}

/// `base64url_no_pad(sha256(verifier))`: the PKCE `S256` code-challenge
/// derivation (RFC 7636 §4.2), computed inline for tests rather than
/// depending on a `pub` helper in `soul_server::auth::oauth` (that module
/// only exports the verifier side, `verify_pkce_s256`).
fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn post_form(uri: &str, body: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

/// Pulls `key`'s value out of a `?a=1&b=2`-style query string (as found in
/// a `Location` header), unescaped. Panics if absent: every call site
/// asserts presence anyway.
fn query_param<'a>(url: &'a str, key: &str) -> &'a str {
    let query = url.split_once('?').expect("url has a query string").1;
    query
        .split('&')
        .find_map(|pair| {
            pair.split_once('=')
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| v)
        })
        .unwrap_or_else(|| panic!("query param {key} not found in {url}"))
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec()
}

#[tokio::test]
async fn ping_reports_version_and_index_version() {
    let (router, _dir, services, _signer) = test_router();

    let response = router
        .oneshot(
            Request::builder()
                .uri("/v0/ping")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
    assert_eq!(body["server"], soul_server::version());
    assert_eq!(body["index_version"], services.index.version().0);
}

/// Drives the standards-based OAuth2/PKCE login flow this server now
/// requires for `/v0` access: `POST /authorize` with the seeded test user's
/// credentials (`"u"`/`"p"`), follow the `302` to pull the authorization
/// `code` out of the `Location` header, then `POST /token` to redeem it for
/// a JWT access token. This replaces the old `/v0/auth` opaque-token helper,
/// `/v0` now only accepts a JWT minted by this server's own `Signer`.
async fn login(router: &Router) -> String {
    let verifier = "fixed-test-code-verifier-0123456789abcdefghijk";
    let challenge = pkce_challenge(verifier);
    let redirect_uri = "http://127.0.0.1:12345/callback";
    let login_form = format!(
        "username=u&password=p&response_type=code&client_id=soul-player\
         &redirect_uri={redirect_uri}&code_challenge={challenge}\
         &code_challenge_method=S256&state=xyz"
    );

    let response = router
        .clone()
        .oneshot(post_form("/authorize", login_form))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("Location header present")
        .to_str()
        .unwrap()
        .to_string();
    let code = query_param(&location, "code").to_string();

    let token_form = format!(
        "grant_type=authorization_code&code={code}&code_verifier={verifier}\
         &redirect_uri={redirect_uri}&client_id=soul-player"
    );
    let response = router
        .clone()
        .oneshot(post_form("/token", token_form))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
    body["access_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn index_requires_auth_and_honors_if_none_match() {
    let (router, _dir, _services, _signer) = test_router();

    let unauthorized = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v0/index")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let token = login(&router).await;

    let authorized = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v0/index")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let etag = authorized
        .headers()
        .get(header::ETAG)
        .expect("ETag header present")
        .to_str()
        .unwrap()
        .to_string();
    let bytes = body_bytes(authorized).await;
    assert!(!bytes.is_empty(), "index body should be non-empty");

    let not_modified = router
        .oneshot(
            Request::builder()
                .uri("/v0/index")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::IF_NONE_MATCH, etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn index_rejects_a_garbage_bearer_token() {
    let (router, _dir, _services, _signer) = test_router();

    let response = router
        .oneshot(
            Request::builder()
                .uri("/v0/index")
                .header(header::AUTHORIZATION, "Bearer not-a-real-jwt-at-all")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn index_rejects_a_jwt_signed_by_the_wrong_key() {
    let (router, _dir, _services, _signer) = test_router();

    // An independent `Signer` (different keypair, same issuer string): a
    // token it mints has a well-formed `iss`/`aud`/`exp`, but a signature
    // this server's own `Signer` never produced.
    let other_dir = tempfile::tempdir().unwrap();
    let other_signer = Signer::load_or_create(other_dir.path(), "http://test.invalid".to_string())
        .expect("load_or_create a second, throwaway signer");
    let forged = other_signer
        .mint(1, 3600, now_unix())
        .expect("mint with the wrong key");

    let response = router
        .oneshot(
            Request::builder()
                .uri("/v0/index")
                .header(header::AUTHORIZATION, format!("Bearer {forged}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn track_range_returns_partial_content_matching_file_head() {
    let (router, _dir, services, _signer) = test_router();
    let token = login(&router).await;

    let path = services.index.locate(0).expect("track 0 resolves");
    let file_bytes = std::fs::read(&path).expect("read the located file directly");

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v0/track/0")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::RANGE, "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert!(
        response.headers().get(header::CONTENT_RANGE).is_some(),
        "expected a Content-Range header on a 206 response"
    );
    let body = body_bytes(response).await;
    assert_eq!(body.len(), 4);
    assert_eq!(body, &file_bytes[0..4]);

    let missing = router
        .oneshot(
            Request::builder()
                .uri("/v0/track/999999")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openid_configuration_is_unauthenticated_and_well_formed() {
    let (router, _dir, _services, _signer) = test_router();

    let response = router
        .oneshot(
            Request::builder()
                .uri("/.well-known/openid-configuration")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
    assert_eq!(body["issuer"], "http://test.invalid");
    assert!(
        body["authorization_endpoint"]
            .as_str()
            .is_some_and(|s| s.ends_with("/authorize")),
        "expected authorization_endpoint to end with /authorize, got {body}"
    );
    assert!(
        body["token_endpoint"]
            .as_str()
            .is_some_and(|s| s.ends_with("/token")),
        "expected token_endpoint to end with /token, got {body}"
    );
    assert!(
        body["revocation_endpoint"]
            .as_str()
            .is_some_and(|s| s.ends_with("/revoke")),
        "expected revocation_endpoint to end with /revoke, got {body}"
    );
    assert!(
        body["jwks_uri"]
            .as_str()
            .is_some_and(|s| s.ends_with("/jwks")),
        "expected jwks_uri to end with /jwks, got {body}"
    );
    assert_eq!(
        body["response_types_supported"],
        serde_json::json!(["code"])
    );
    let grant_types = body["grant_types_supported"]
        .as_array()
        .expect("grant_types_supported is an array");
    assert!(grant_types.contains(&serde_json::json!("authorization_code")));
    assert!(grant_types.contains(&serde_json::json!("refresh_token")));
    assert_eq!(
        body["code_challenge_methods_supported"],
        serde_json::json!(["S256"])
    );
    assert_eq!(
        body["token_endpoint_auth_methods_supported"],
        serde_json::json!(["none"])
    );
    assert_eq!(body["scopes_supported"], serde_json::json!(["library"]));
    assert_eq!(
        body["subject_types_supported"],
        serde_json::json!(["public"])
    );
    assert_eq!(
        body["id_token_signing_alg_values_supported"],
        serde_json::json!(["EdDSA"])
    );
}

#[tokio::test]
async fn jwks_is_unauthenticated_and_exposes_the_signing_key() {
    let (router, _dir, _services, _signer) = test_router();

    let response = router
        .oneshot(Request::builder().uri("/jwks").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
    let key = &body["keys"][0];
    assert_eq!(key["kty"], "OKP");
    assert_eq!(key["alg"], "EdDSA");
    assert!(
        key["kid"].as_str().is_some_and(|s| !s.is_empty()),
        "expected a non-empty kid, got {body}"
    );
}

// ---------------------------------------------------------------------------
// OAuth2/PKCE: GET+POST /authorize, POST /token, POST /revoke
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oauth_authorization_code_and_refresh_flow_end_to_end() {
    let (router, _dir, _services, signer) = test_router();

    let verifier = "fixed-test-code-verifier-0123456789abcdefghijk";
    let challenge = pkce_challenge(verifier);
    let redirect_uri = "http://127.0.0.1:12345/callback";
    let login_form = format!(
        "username=u&password=p&response_type=code&client_id=soul-player\
         &redirect_uri={redirect_uri}&code_challenge={challenge}\
         &code_challenge_method=S256&state=xyz"
    );

    // Step 1: GET /authorize -> 200 HTML containing a password field.
    let get_uri = format!(
        "/authorize?response_type=code&client_id=soul-player\
         &redirect_uri={redirect_uri}&code_challenge={challenge}\
         &code_challenge_method=S256&state=xyz&scope=library"
    );
    let response = router
        .clone()
        .oneshot(Request::builder().uri(get_uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(body_bytes(response).await).unwrap();
    assert!(
        html.contains(r#"type="password""#),
        "expected a password field, got {html}"
    );

    // Step 2: POST /authorize with correct creds -> 302 to redirect_uri with
    // a code + the echoed state.
    let response = router
        .clone()
        .oneshot(post_form("/authorize", login_form.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("Location header present")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        location.starts_with(&format!("{redirect_uri}?")),
        "expected redirect back to {redirect_uri}, got {location}"
    );
    assert_eq!(query_param(&location, "state"), "xyz");
    let code = query_param(&location, "code").to_string();

    // Step 3: POST /token (authorization_code) -> 200 access+refresh; the
    // access token verifies via the server's own Signer.
    let token_form = format!(
        "grant_type=authorization_code&code={code}&code_verifier={verifier}\
         &redirect_uri={redirect_uri}&client_id=soul-player"
    );
    let response = router
        .clone()
        .oneshot(post_form("/token", token_form))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
    let access_token = body["access_token"].as_str().unwrap().to_string();
    let refresh_token = body["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["scope"], "library");
    assert!(body["expires_in"].as_u64().unwrap() > 0);
    assert!(
        signer.verify(&access_token, now_unix()).is_some(),
        "access token minted by /token must verify under the server's Signer"
    );

    // Step 4: POST /token (refresh_token) -> 200 with a NEW access + NEW
    // refresh token; replaying the old refresh token afterwards -> 400.
    let refresh_form = format!("grant_type=refresh_token&refresh_token={refresh_token}");
    let response = router
        .clone()
        .oneshot(post_form("/token", refresh_form.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
    let rotated_access = body["access_token"].as_str().unwrap().to_string();
    let rotated_refresh = body["refresh_token"].as_str().unwrap().to_string();
    // Note: the access token itself isn't asserted distinct from the one in
    // step 3, `Signer::mint` is deterministic on (user, ttl, iat, exp), so
    // two mints within the same wall-clock second are legitimately
    // byte-identical. Rotation's real guarantee is the refresh token below.
    assert_ne!(rotated_refresh, refresh_token);
    assert!(signer.verify(&rotated_access, now_unix()).is_some());

    let replay = router
        .clone()
        .oneshot(post_form("/token", refresh_form))
        .await
        .unwrap();
    assert_eq!(
        replay.status(),
        StatusCode::BAD_REQUEST,
        "replaying a rotated-away refresh token must fail"
    );

    // Step 5: POST /authorize with a wrong password -> 200 (form again, no
    // code/redirect).
    let wrong_password_form = login_form.replace("password=p&", "password=WRONG&");
    let response = router
        .clone()
        .oneshot(post_form("/authorize", wrong_password_form))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::LOCATION).is_none());
    let html = String::from_utf8(body_bytes(response).await).unwrap();
    assert!(html.contains(r#"type="password""#));

    // Step 6: a tampered code_verifier against a freshly issued code -> 400.
    let response = router
        .clone()
        .oneshot(post_form("/authorize", login_form.clone()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let fresh_code = query_param(&location, "code").to_string();
    let tampered_form = format!(
        "grant_type=authorization_code&code={fresh_code}&code_verifier=not-the-right-verifier\
         &redirect_uri={redirect_uri}&client_id=soul-player"
    );
    let response = router
        .clone()
        .oneshot(post_form("/token", tampered_form))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Step 7: POST /revoke with the (still-live) rotated refresh -> 200;
    // using it for a refresh grant afterwards -> 400.
    let revoke_form = format!("token={rotated_refresh}");
    let response = router
        .clone()
        .oneshot(post_form("/revoke", revoke_form))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let refresh_after_revoke = format!("grant_type=refresh_token&refresh_token={rotated_refresh}");
    let response = router
        .oneshot(post_form("/token", refresh_after_revoke))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn authorize_rejects_open_redirect_to_non_loopback_uri() {
    let (router, _dir, _services, _signer) = test_router();

    let challenge = pkce_challenge("irrelevant-verifier-for-this-test");
    let uri = format!(
        "/authorize?response_type=code&client_id=soul-player\
         &redirect_uri=http://evil.com/callback&code_challenge={challenge}\
         &code_challenge_method=S256&state=xyz"
    );
    let response = router
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = String::from_utf8(body_bytes(response).await).unwrap();
    assert!(
        !body.contains(r#"type="password""#),
        "an open-redirect rejection must not render the login form, got {body}"
    );
}

/// `/token` must bind the authorization_code grant to the same client the code
/// was issued to. Redeeming a code with a different `client_id` than the one
/// `/authorize` required must be rejected, not silently honored.
#[tokio::test]
async fn token_rejects_a_mismatched_client_id() {
    let (router, _dir, _services, _signer) = test_router();

    let verifier = "fixed-test-code-verifier-0123456789abcdefghijk";
    let challenge = pkce_challenge(verifier);
    let redirect_uri = "http://127.0.0.1:12345/callback";
    let login_form = format!(
        "username=u&password=p&response_type=code&client_id=soul-player\
         &redirect_uri={redirect_uri}&code_challenge={challenge}\
         &code_challenge_method=S256&state=xyz"
    );
    let response = router
        .clone()
        .oneshot(post_form("/authorize", login_form))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let code = query_param(&location, "code").to_string();

    let token_form = format!(
        "grant_type=authorization_code&code={code}&code_verifier={verifier}\
         &redirect_uri={redirect_uri}&client_id=some-other-client"
    );
    let response = router
        .oneshot(post_form("/token", token_form))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a /token request whose client_id does not match the code's client must be rejected"
    );
}

/// A `redirect_uri` whose authority is loopback (so it passes the open-redirect
/// check) but whose path carries CRLF bytes (illegal in an HTTP header value)
/// must not panic the handler when it builds the `Location` header. The server
/// returns a clean error response instead of a panicked, reset request.
#[tokio::test]
async fn authorize_does_not_panic_on_a_redirect_uri_with_control_bytes() {
    let (router, _dir, _services, _signer) = test_router();

    let verifier = "fixed-test-code-verifier-0123456789abcdefghijk";
    let challenge = pkce_challenge(verifier);
    // `%0D%0A` decodes (via the Form extractor) to a real CR LF in the path.
    let redirect_uri = "http://127.0.0.1:12345/%0D%0AInjected";
    let login_form = format!(
        "username=u&password=p&response_type=code&client_id=soul-player\
         &redirect_uri={redirect_uri}&code_challenge={challenge}\
         &code_challenge_method=S256&state=xyz"
    );
    let response = router
        .oneshot(post_form("/authorize", login_form))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a redirect_uri with header-illegal bytes is rejected cleanly, not via a panic"
    );
}

/// Find the track id whose backing file is largest, so a test exercises a body
/// that spans several stream chunks (the prod chunk is 64 KiB; testdata holds
/// files into the hundreds of KiB).
fn largest_track(services: &AppServices) -> (u32, std::path::PathBuf, u64) {
    let mut best: Option<(u32, std::path::PathBuf, u64)> = None;
    for id in 0..64u32 {
        if let Some(path) = services.index.locate(id) {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if best.as_ref().is_none_or(|(_, _, b)| size > *b) {
                best = Some((id, path, size));
            }
        }
    }
    best.expect("at least one track in testdata")
}

/// A full (no-Range) track response streams the whole file, byte-identical, with
/// a matching Content-Length, across multiple chunk boundaries.
#[tokio::test]
async fn track_streams_a_full_file_byte_identical() {
    let (router, _dir, services, _signer) = test_router();
    let token = login(&router).await;

    let (id, path, size) = largest_track(&services);
    assert!(
        size > 64 * 1024,
        "the largest testdata track ({size} B) should span multiple 64 KiB chunks"
    );
    let file_bytes = std::fs::read(&path).expect("read the located file directly");

    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/v0/track/{id}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok()),
        Some(size.to_string().as_str()),
        "Content-Length is the full file size"
    );
    let body = body_bytes(response).await;
    assert_eq!(body.len() as u64, size, "the whole file was streamed");
    assert_eq!(
        body, file_bytes,
        "streamed bytes are byte-identical to the file"
    );
}

/// A Range request that spans multiple chunks streams exactly that slice,
/// byte-identical, with a correct Content-Range and Content-Length.
#[tokio::test]
async fn track_streams_a_multi_chunk_range_byte_identical() {
    let (router, _dir, services, _signer) = test_router();
    let token = login(&router).await;

    let (id, path, size) = largest_track(&services);
    let file_bytes = std::fs::read(&path).expect("read the located file directly");

    // A window that starts off a chunk boundary and runs past several chunks.
    let start: u64 = 100;
    let end: u64 = size - 2;
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/v0/track/{id}"))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::RANGE, format!("bytes={start}-{end}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok()),
        Some(format!("bytes {start}-{end}/{size}").as_str()),
    );
    let body = body_bytes(response).await;
    assert_eq!(
        body,
        &file_bytes[start as usize..=end as usize],
        "the streamed slice matches the file window exactly"
    );
}
