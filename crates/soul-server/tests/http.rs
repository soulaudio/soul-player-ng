//! HTTP-level tests for the native `/v0` protocol, exercised through
//! [`tower::ServiceExt::oneshot`] over a real `Services` (no socket bound):
//! a `CoreIndex` scanning the repo's `testdata/` fixtures, the real
//! `FsMedia`, and a temp-file `FileUsers` with one seeded user.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use soul_server::domain::ports::{IndexPort, UserStore};
use soul_server::domain::service::Services;
use soul_server::index::CoreIndex;
use soul_server::media::FsMedia;
use soul_server::proto::native::{self, AppServices};
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
fn test_router() -> (Router, tempfile::TempDir, Arc<AppServices>) {
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
    let router = native::router(services.clone());
    (router, dir, services)
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
    let (router, _dir, services) = test_router();

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

#[tokio::test]
async fn auth_accepts_right_password_and_rejects_wrong_password() {
    let (router, _dir, _services) = test_router();

    let ok = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v0/auth")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"user":"u","pass":"p"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(ok).await).unwrap();
    assert!(
        body["token"].as_str().is_some_and(|t| !t.is_empty()),
        "expected a non-empty token, got {body}"
    );

    let bad = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v0/auth")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"user":"u","pass":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);
}

async fn authenticate(router: &Router) -> String {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v0/auth")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"user":"u","pass":"p"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_bytes(response).await).unwrap();
    body["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn index_requires_auth_and_honors_if_none_match() {
    let (router, _dir, _services) = test_router();

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

    let token = authenticate(&router).await;

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
async fn track_range_returns_partial_content_matching_file_head() {
    let (router, _dir, services) = test_router();
    let token = authenticate(&router).await;

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
