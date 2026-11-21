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
use axum::extract::{Form, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, async_trait};
use serde::{Deserialize, Serialize};

use crate::auth::oauth::{CodeStore, is_loopback_redirect};
use crate::auth::signer::Signer;
use crate::domain::ports::IndexPort;
use crate::domain::service::Services;
use crate::domain::{DomainError, UserId};
use crate::index::CoreIndex;
use crate::media::FsMedia;
use crate::store::FileUsers;

/// The concrete `Services` instantiation the native protocol serves —
/// the real core/filesystem/flat-file adapters, not the domain layer's test
/// fakes.
pub type AppServices = Services<CoreIndex, FsMedia, FileUsers>;

/// Router/extractor state: the shared `Services`, plus the pieces needed to
/// mint and verify OIDC-standard tokens — the signing key, this server's
/// `issuer` (its own base URL, used both to self-describe in discovery and
/// to stamp/validate the `iss` claim), and the access-token lifetime later
/// token-issuing handlers mint against.
#[derive(Clone)]
pub struct AppState {
    pub services: Arc<AppServices>,
    pub signer: Arc<Signer>,
    pub issuer: String,
    pub access_ttl: u64,
    /// In-memory, single-use authorization-code store for the PKCE
    /// `/authorize` → `/token` handoff. Constructed fresh here rather than
    /// threaded through as a constructor parameter: codes are short-lived
    /// (60s) and process-local by design, so every `AppState` simply gets
    /// its own empty store, and no existing caller of `new` has to change.
    pub code_store: Arc<CodeStore>,
}

impl AppState {
    pub fn new(
        services: Arc<AppServices>,
        signer: Arc<Signer>,
        issuer: String,
        access_ttl: u64,
    ) -> Self {
        Self {
            services,
            signer,
            issuer,
            access_ttl,
            code_store: Arc::new(CodeStore::new()),
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = AppServices;

    fn deref(&self) -> &Self::Target {
        &self.services
    }
}

/// Build the `/v0` router, plus the unauthenticated OIDC discovery and JWKS
/// endpoints, over `state`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v0/ping", get(ping))
        .route("/v0/index", get(index))
        .route("/v0/track/:id", get(track))
        .route("/v0/art/:album_id", get(art))
        .route(
            "/.well-known/openid-configuration",
            get(openid_configuration),
        )
        .route("/jwks", get(jwks))
        .route("/authorize", get(authorize_get).post(authorize_post))
        .route("/token", post(token))
        .route("/revoke", post(revoke))
        .with_state(state)
}

/// Bind `bind` and serve the router until the process stops.
pub async fn serve(bind: &str, state: AppState) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, router(state)).await?;
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
        let claims = state
            .signer
            .verify(token, now_unix())
            .ok_or(StatusCode::UNAUTHORIZED)?;
        let user_id: UserId = claims.sub.parse().map_err(|_| StatusCode::UNAUTHORIZED)?;
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

// ---------------------------------------------------------------------------
// GET /.well-known/openid-configuration — no auth (it's the discovery doc)
// ---------------------------------------------------------------------------

/// OIDC discovery document. Advertises this server's token/authorization
/// endpoints and the subset of the spec it actually implements: an
/// authorization-code flow with PKCS (`S256` only, no client secret — this
/// is a public-client, native-app deployment) and refresh tokens, signed
/// with the single `EdDSA` key `/jwks` publishes.
async fn openid_configuration(State(state): State<AppState>) -> Json<serde_json::Value> {
    let issuer = &state.issuer;
    Json(serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "revocation_endpoint": format!("{issuer}/revoke"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["library"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["EdDSA"],
    }))
}

// ---------------------------------------------------------------------------
// GET /jwks — no auth (it's how clients verify tokens)
// ---------------------------------------------------------------------------

async fn jwks(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.signer.jwks())
}

// ---------------------------------------------------------------------------
// GET+POST /authorize, POST /token, POST /revoke — the OAuth2/PKCE
// Authorization Server surface. No auth required (this *is* how you get
// one); the security boundary is the validation inside each handler, not
// the `Authed` extractor.
// ---------------------------------------------------------------------------

/// Current time as unix seconds — the real authorization server's clock, so
/// expiry checks are always against actual wall-clock time (not a
/// test-injected value, unlike the pure [`CodeStore`]/`RefreshStore` logic
/// those handlers call into).
fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

/// Lifetime (seconds) of a freshly issued authorization code. Deliberately
/// short — the code only has to survive the single redirect from this
/// server's login form back to the client's loopback listener.
const AUTH_CODE_TTL_SECS: u64 = 60;

/// `Some(s)` form of `opt` with empty strings treated as absent — a client
/// that sends `redirect_uri=` is, for validation purposes, the same as one
/// that omits `redirect_uri` entirely.
fn non_empty(opt: &Option<String>) -> Option<&str> {
    opt.as_deref().filter(|s| !s.is_empty())
}

/// Percent-encodes `s` for safe inclusion as a query-string value appended
/// to a redirect URI (RFC 3986 unreserved set passes through unescaped;
/// everything else becomes `%XX`). Written by hand instead of pulling in a
/// URL-encoding crate — `code` and `state` are the only values this server
/// ever appends this way.
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

/// Escapes `s` for safe embedding as HTML text/attribute content — the
/// minimal set RFC-wise unsafe characters, since the login form is a plain
/// `format!` string rather than a template engine that would do this for
/// us automatically.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Query/form parameters shared by `GET /authorize` (query string) and
/// `POST /authorize` (form body, plus `username`/`password`). One struct
/// serves both: `Query` and `Form` both deserialize missing `Option` fields
/// as `None`, so the GET path simply never populates `username`/`password`.
#[derive(Deserialize)]
struct AuthorizeParams {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    scope: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

/// An `/authorize` request whose OAuth parameters have all passed
/// validation — safe to render into a login form or exchange for a code.
struct ValidAuthorize {
    redirect_uri: String,
    code_challenge: String,
    state: String,
    scope: Option<String>,
}

/// Validates `/authorize` parameters in the exact order the spec requires.
/// On ANY failure, returns the error message for a direct `400` (plain
/// text) rather than ever bouncing back to the caller-supplied
/// `redirect_uri` — an unvalidated redirect target is an open-redirect
/// primitive, so `redirect_uri` itself is validated (step 3) before
/// anything is rendered, and no failure path here ever produces a `3xx`
/// response. (Returns the message, not a built [`Response`]: a `Response`
/// `Err` variant is 100+ bytes, which clippy's `result_large_err` flags —
/// the caller turns this into a `400` via [`bad_request`].)
fn validate_authorize(params: &AuthorizeParams) -> Result<ValidAuthorize, &'static str> {
    if params.response_type.as_deref() != Some("code") {
        return Err("unsupported response_type: expected \"code\"");
    }
    if params.client_id.as_deref() != Some("soul-player") {
        return Err("unknown client_id");
    }
    let redirect_uri = match non_empty(&params.redirect_uri) {
        Some(uri) if is_loopback_redirect(uri) => uri.to_string(),
        _ => return Err("redirect_uri must be a loopback http:// URI (127.0.0.1 or [::1])"),
    };
    let code_challenge = match non_empty(&params.code_challenge) {
        Some(challenge) => challenge.to_string(),
        None => return Err("missing code_challenge"),
    };
    if params.code_challenge_method.as_deref() != Some("S256") {
        return Err("code_challenge_method must be \"S256\"");
    }
    let state = match non_empty(&params.state) {
        Some(state) => state.to_string(),
        None => return Err("missing state"),
    };

    Ok(ValidAuthorize {
        redirect_uri,
        code_challenge,
        state,
        scope: params.scope.clone(),
    })
}

/// A direct, plain-text `400` — never a redirect.
fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, message.to_string()).into_response()
}

/// Renders the minimal login form: a username/password pair that POSTs back
/// to `/authorize`, with every oauth parameter echoed as a hidden,
/// HTML-escaped field so the POST handler can re-validate them (never
/// trusting the redirect target just because it was already validated once
/// on the GET).
fn render_login_form(valid: &ValidAuthorize, error: Option<&str>) -> Response {
    let error_html = match error {
        Some(message) => format!(r#"<p class="error">{}</p>"#, html_escape(message)),
        None => String::new(),
    };
    let scope_field = match &valid.scope {
        Some(scope) => format!(
            r#"<input type="hidden" name="scope" value="{}">"#,
            html_escape(scope)
        ),
        None => String::new(),
    };

    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in</title>
<style>
:root {{ color-scheme: light dark; }}
body {{ font-family: system-ui,-apple-system,"Segoe UI",Roboto,sans-serif; margin:0; min-height:100vh; display:flex; align-items:center; justify-content:center; background:#f4f4f6; }}
.card {{ width:min(360px,90vw); box-sizing:border-box; padding:26px 28px 24px; border-radius:14px; background:#fff; box-shadow:0 8px 30px rgba(0,0,0,.12); }}
h1 {{ font-size:1.15rem; margin:0 0 2px; }}
.sub {{ margin:0 0 6px; opacity:.6; font-size:.82rem; }}
label {{ display:block; font-size:.78rem; opacity:.75; margin:14px 0 5px; }}
input {{ display:block; width:100%; box-sizing:border-box; padding:10px 12px; border-radius:9px; border:1px solid rgba(128,128,128,.4); background:transparent; color:inherit; font-size:.95rem; }}
input:focus {{ outline:2px solid #8b7ff0; outline-offset:1px; border-color:transparent; }}
button {{ margin-top:20px; width:100%; padding:11px; border:0; border-radius:9px; background:#8b7ff0; color:#fff; font-size:.95rem; font-weight:600; cursor:pointer; }}
button:hover {{ background:#796ced; }}
.error {{ color:#d9534f; font-size:.85rem; margin:8px 0 0; }}
@media (prefers-color-scheme: dark) {{
  body {{ background:#17171c; color:#e8e8ee; }}
  .card {{ background:#22222a; box-shadow:0 8px 30px rgba(0,0,0,.45); }}
}}
</style>
</head>
<body>
<form class="card" method="post" action="/authorize">
<h1>Sign in to soul-player</h1>
<p class="sub">Connecting a remote source</p>
{error_html}
<input type="hidden" name="response_type" value="code">
<input type="hidden" name="client_id" value="soul-player">
<input type="hidden" name="redirect_uri" value="{redirect_uri}">
<input type="hidden" name="code_challenge" value="{code_challenge}">
<input type="hidden" name="code_challenge_method" value="S256">
<input type="hidden" name="state" value="{state}">
{scope_field}
<label for="u">Username</label>
<input id="u" type="text" name="username" autocomplete="username" autofocus required>
<label for="p">Password</label>
<input id="p" type="password" name="password" autocomplete="current-password" required>
<button type="submit">Sign in</button>
</form>
</body>
</html>
"#,
        error_html = error_html,
        redirect_uri = html_escape(&valid.redirect_uri),
        code_challenge = html_escape(&valid.code_challenge),
        state = html_escape(&valid.state),
        scope_field = scope_field,
    ))
    .into_response()
}

async fn authorize_get(Query(params): Query<AuthorizeParams>) -> Response {
    match validate_authorize(&params) {
        Ok(valid) => render_login_form(&valid, None),
        Err(message) => bad_request(message),
    }
}

async fn authorize_post(
    State(state): State<AppState>,
    Form(params): Form<AuthorizeParams>,
) -> Response {
    let valid = match validate_authorize(&params) {
        Ok(valid) => valid,
        Err(message) => return bad_request(message),
    };

    let username = params.username.as_deref().unwrap_or_default();
    let password = params.password.as_deref().unwrap_or_default();

    match state.verify_credentials(username, password) {
        Some(user) => {
            let now = now_unix();
            let code = state.code_store.issue(
                user,
                valid.code_challenge.clone(),
                valid.redirect_uri.clone(),
                now,
                AUTH_CODE_TTL_SECS,
            );
            let location = format!(
                "{}?code={}&state={}",
                valid.redirect_uri,
                percent_encode(&code),
                percent_encode(&valid.state),
            );
            Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, location)
                .body(Body::empty())
                .expect("static headers + empty body always build")
        }
        None => render_login_form(&valid, Some("Incorrect username or password.")),
    }
}

#[derive(Deserialize)]
struct TokenParams {
    grant_type: Option<String>,
    code: Option<String>,
    code_verifier: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Serialize)]
struct TokenSuccess {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
    refresh_token: String,
    scope: &'static str,
}

#[derive(Serialize)]
struct OauthErrorBody {
    error: &'static str,
}

/// A standard OAuth error-response body (RFC 6749 §5.2), e.g.
/// `{"error":"invalid_grant"}`.
fn oauth_error(status: StatusCode, error: &'static str) -> Response {
    (status, Json(OauthErrorBody { error })).into_response()
}

/// Mints a fresh access token for `user` and assembles the success JSON
/// body paired with `refresh_token` (either a brand-new one from the
/// `authorization_code` grant, or a just-rotated one from `refresh_token`).
fn mint_token_pair(state: &AppState, user: UserId, refresh_token: String, now: u64) -> Response {
    match state.signer.mint(user, state.access_ttl, now) {
        Ok(access_token) => Json(TokenSuccess {
            access_token,
            token_type: "Bearer",
            expires_in: state.access_ttl,
            refresh_token,
            scope: "library",
        })
        .into_response(),
        Err(_) => oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error"),
    }
}

async fn token(State(state): State<AppState>, Form(params): Form<TokenParams>) -> Response {
    let now = now_unix();

    match params.grant_type.as_deref() {
        Some("authorization_code") => {
            let (Some(code), Some(verifier), Some(redirect_uri), Some(_client_id)) = (
                non_empty(&params.code),
                non_empty(&params.code_verifier),
                non_empty(&params.redirect_uri),
                non_empty(&params.client_id),
            ) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
            };

            let Some(user) = state.code_store.consume(code, verifier, redirect_uri, now) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
            };

            let refresh_token = match state.issue_refresh(user, now) {
                Ok(token) => token,
                Err(_) => return oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error"),
            };

            mint_token_pair(&state, user, refresh_token, now)
        }
        Some("refresh_token") => {
            let Some(refresh_token) = non_empty(&params.refresh_token) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
            };

            let Some((user, new_refresh)) = state.rotate_refresh(refresh_token, now) else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
            };

            mint_token_pair(&state, user, new_refresh, now)
        }
        _ => oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type"),
    }
}

#[derive(Deserialize)]
struct RevokeParams {
    token: Option<String>,
    /// RFC 7009 token-type hint. Accepted but unused: `revoke_refresh` just
    /// tries the refresh-token store directly, and this server has no other
    /// token kind worth hinting at.
    #[allow(dead_code)]
    token_type_hint: Option<String>,
}

/// Always `200`, regardless of whether `token` matched anything — RFC 7009
/// §2.2 requires the revocation endpoint not to leak whether a presented
/// token was valid.
async fn revoke(State(state): State<AppState>, Form(params): Form<RevokeParams>) -> StatusCode {
    if let Some(token) = non_empty(&params.token) {
        let _ = state.revoke_refresh(token);
    }
    StatusCode::OK
}
