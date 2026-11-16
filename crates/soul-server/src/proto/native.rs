//! Native `/v0` HTTP protocol: the server's first-class inbound adapter.
//!
//! An [`axum`] [`Router`] that wraps [`Services`] with wire framing only —
//! ping, bearer auth, index sync (with `ETag`/`If-None-Match`), Range-capable
//! track streaming, and a cover-art stub. No business logic lives here; every
//! handler just translates an HTTP request into a `Services` call and a
//! [`DomainError`] (or success) into a response.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, async_trait};
use serde::{Deserialize, Serialize};

use crate::domain::ports::IndexPort;
use crate::domain::service::Services;
use crate::domain::{DomainError, UserId};
use crate::index::CoreIndex;
use crate::media::FsMedia;
use crate::store::SqliteUsers;

/// The concrete `Services` instantiation the native protocol serves —
/// the real core/filesystem/sqlite adapters, not the domain layer's test
/// fakes.
pub type AppServices = Services<CoreIndex, FsMedia, SqliteUsers>;

/// Router/extractor state: one shared `Services` for the process lifetime.
type AppState = Arc<AppServices>;

/// Build the `/v0` router over `services`.
pub fn router(services: Arc<AppServices>) -> Router {
    Router::new()
        .route("/v0/ping", get(ping))
        .route("/v0/auth", post(auth))
        .route("/v0/index", get(index))
        .route("/v0/track/:id", get(track))
        .route("/v0/art/:album_id", get(art))
        .with_state(services)
}

/// Bind `bind` and serve the `/v0` router until the process stops.
pub async fn serve(bind: &str, services: Arc<AppServices>) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, router(services)).await?;
    Ok(())
}

/// Map a [`DomainError`] to its HTTP status. `v0` doesn't leak `Other`'s
/// internals to clients — it's an opaque 500.
fn error_status(error: &DomainError) -> StatusCode {
    match error {
        DomainError::Unauthorized => StatusCode::UNAUTHORIZED,
        DomainError::NotFound => StatusCode::NOT_FOUND,
        DomainError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

// ---------------------------------------------------------------------------
// Bearer-token auth extractor
// ---------------------------------------------------------------------------

/// A request authenticated via `Authorization: Bearer <token>`, resolved to
/// the [`UserId`] the token was issued for. Any handler taking this as an
/// argument rejects a missing or invalid token with `401` before the handler
/// body runs.
struct Authed(#[allow(dead_code)] UserId);

#[async_trait]
impl axum::extract::FromRequestParts<AppState> for Authed {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer_token(&parts.headers).ok_or(StatusCode::UNAUTHORIZED)?;
        let user_id = state.check(token).map_err(|_| StatusCode::UNAUTHORIZED)?;
        Ok(Authed(user_id))
    }
}

/// Pull the token out of an `Authorization: Bearer <token>` header, if
/// present and well-formed.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

// ---------------------------------------------------------------------------
// GET /v0/ping — no auth
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct PingResponse {
    server: &'static str,
    index_version: u64,
}

async fn ping(State(services): State<AppState>) -> impl IntoResponse {
    Json(PingResponse {
        server: crate::version(),
        index_version: services.index.version().0,
    })
}

// ---------------------------------------------------------------------------
// POST /v0/auth — no auth (it's how you get one)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AuthRequest {
    user: String,
    pass: String,
}

#[derive(Serialize)]
struct AuthResponse {
    token: String,
}

async fn auth(State(services): State<AppState>, Json(body): Json<AuthRequest>) -> Response {
    match services.authenticate(&body.user, &body.pass) {
        Ok(token) => Json(AuthResponse { token: token.0 }).into_response(),
        Err(error) => error_status(&error).into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /v0/index — auth required
// ---------------------------------------------------------------------------

async fn index(State(services): State<AppState>, _auth: Authed, headers: HeaderMap) -> Response {
    let (bytes, version) = match services.index() {
        Ok(ok) => ok,
        Err(error) => return error_status(&error).into_response(),
    };
    let etag = format!("\"{}\"", version.0);

    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == etag);
    if not_modified {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .body(Body::empty())
            .expect("static headers + empty body always build");
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::ETAG, etag)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from(bytes))
        .expect("static headers + known body always build")
}

// ---------------------------------------------------------------------------
// GET /v0/track/{id} — auth required, Range-capable
// ---------------------------------------------------------------------------

/// Parse a single-range `Range: bytes=START-END` header value. `END` is
/// optional (an open range to end-of-file). Suffix ranges (`bytes=-500`) and
/// multi-range requests aren't supported in v0 — both fail to parse and fall
/// back to a full response, same as no `Range` header at all.
fn parse_range(value: &str) -> Option<(u64, Option<u64>)> {
    let spec = value.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start, end))
}

async fn track(
    State(services): State<AppState>,
    Path(id): Path<u32>,
    _auth: Authed,
    headers: HeaderMap,
) -> Response {
    let (mut reader, len) = match services.open_track(id) {
        Ok(ok) => ok,
        Err(error) => return error_status(&error).into_response(),
    };

    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_range);

    match range {
        Some((start, end_requested)) => {
            let last_byte = len.saturating_sub(1);
            let end = end_requested.unwrap_or(last_byte).min(last_byte);
            if len == 0 || start > end {
                return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            }

            let slice_len = (end - start + 1) as usize;
            let mut buf = vec![0u8; slice_len];
            if let Err(error) = reader
                .seek(SeekFrom::Start(start))
                .and_then(|_| reader.read_exact(&mut buf))
            {
                return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
            }

            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
                .header(header::CONTENT_LENGTH, slice_len)
                .body(Body::from(buf))
                .expect("static headers + known body always build")
        }
        None => {
            let mut buf = Vec::new();
            if let Err(error) = reader.read_to_end(&mut buf) {
                return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
            }
            let content_length = buf.len();

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, content_length)
                .body(Body::from(buf))
                .expect("static headers + known body always build")
        }
    }
}

// ---------------------------------------------------------------------------
// GET /v0/art/{album_id} — auth required, documented 404 stub
// ---------------------------------------------------------------------------

/// Cover-art serving is a documented follow-up (see the design spec's "Out"
/// list): `IndexPort` has no art accessor yet, so there's nothing to look up.
/// Always `404` until that lands.
async fn art(_auth: Authed, Path(_album_id): Path<u32>) -> StatusCode {
    StatusCode::NOT_FOUND
}
