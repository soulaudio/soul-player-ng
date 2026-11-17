# Standards-Based Remote Auth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the bespoke `/v0/auth` + opaque bearer token with a standards-based OAuth 2.1 flow — `soul-server` as a minimal Authorization + Resource Server (discovery, JWKS, `/authorize` login, `/token`, `/revoke`, RS256 JWT access + rotating refresh tokens), and the desktop client logging in via the system browser (Authorization Code + PKCE, loopback redirect) with the refresh token in the OS keychain.

**Architecture:** New `auth` module in `soul-server` (ports `Signer`/`RefreshStore` + the AS endpoints in `proto/native.rs`); `/v0/*` validates the JWT locally. The host `remote` module replaces password auth with a PKCE browser login + a `TokenStore` (keyring). Hexagonal — same ports-and-adapters shape as the rest of the server.

**Tech Stack:** Rust. Server: `jsonwebtoken` (RS256 JWT), `rsa` + `rand` (keygen), `axum`. Client: `oauth2` + `openidconnect` (PKCE/discovery/RP), `keyring` (secrets), `open` (system browser). `argon2` (existing, login verify).

## Global Constraints

- Root workspace edition 2024, rust-version 1.85. `soul-server` is a root member; `crates/soul-player-host` is its OWN workspace (`cargo … --manifest-path crates/soul-player-host/Cargo.toml`). Both forbid `unsafe`; clippy `all = deny`.
- Native protocol version prefix stays `/v0` for the resource API; the AS endpoints are unversioned (`/authorize`, `/token`, `/revoke`, `/jwks`, `/.well-known/openid-configuration`).
- JWT access tokens: **RS256**, claims `iss` (server base URL), `sub` (user id as string), `aud` `"soul-player"`, `exp` (~1 h), `iat`, `scope` `"library"`. Verify sig + `exp` + `iss` + `aud` on `/v0/*`.
- `client_id` is the fixed public client `"soul-player"`; `token_endpoint_auth_methods_supported` = `["none"]`; `code_challenge_methods_supported` = `["S256"]`; `redirect_uri` must be an `http://127.0.0.1:{port}/callback` loopback (RFC 8252 §7.3 — any port on 127.0.0.1).
- Refresh tokens: 32 CSPRNG bytes, stored **hashed** (SHA-256) + durable in the user-store file, rotated on use, revocable.
- Commit messages: conventional; NEVER reference Claude/Anthropic/AI/co-authors (hook rejects). `cargo fmt` + `cargo clippy --all-targets -- -D warnings` clean per commit. New deps must keep `cargo audit`/`cargo deny` green (no advisories / disallowed licenses).
- TDD throughout: failing test first, watch it fail, minimal code, commit.
- Reuse: `store::FileUsers` (Argon2id `verify`/`add_user` stay), `domain::ports`, `domain::service::Services`, `proto::native::{router,serve,Authed}`, host `remote::{RemoteServer,fetch_index,DownloadCache,RemoteLibrary}` (only the auth-token acquisition changes).

## File structure
- `crates/soul-server/src/auth/mod.rs` — `pub mod signer; pub mod refresh; pub mod oauth;` + shared types (`AccessClaims`, `Tokens`, `AuthError`).
- `crates/soul-server/src/auth/signer.rs` — `Signer` (RS256 keypair gen/persist, JWT mint/verify, JWK export).
- `crates/soul-server/src/auth/refresh.rs` — `RefreshStore` (hashed, durable, rotating, revocable).
- `crates/soul-server/src/auth/oauth.rs` — the AS logic: auth-code store, PKCE check, the grant handlers (pure functions over the ports; HTTP wiring stays in `proto/native.rs`).
- `crates/soul-server/src/proto/native.rs` — add `/authorize`, `/token`, `/revoke`, `/jwks`, discovery routes; `Authed` switches to JWT; remove `/v0/auth`.
- `crates/soul-player-host/src/remote.rs` — `login`, `refresh`, `TokenStore` trait + `KeyringStore`/`MemStore`; `RemoteServer` carries the access token.
- `crates/soul-player-host/tests/remote_e2e.rs` — migrated to the new login.

---

## Increment A — crypto foundation (Signer, JWT, JWKS, discovery)

### Task A1: `Signer` — RS256 keypair (persisted) + JWT mint/verify + JWK

**Files:** Create `crates/soul-server/src/auth/mod.rs`, `crates/soul-server/src/auth/signer.rs`; Modify `crates/soul-server/src/lib.rs` (`pub mod auth;`), `crates/soul-server/Cargo.toml`.

**Interfaces — Produces:**
```rust
// auth/mod.rs
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct AccessClaims { pub iss: String, pub sub: String, pub aud: String, pub exp: usize, pub iat: usize, pub scope: String }
// auth/signer.rs
pub struct Signer { /* encoding+decoding keys, kid, issuer */ }
impl Signer {
    pub fn load_or_create(dir: &std::path::Path, issuer: String) -> anyhow::Result<Self>; // RSA-2048 PEM at dir/jwt_key.pem (0600), created if absent
    pub fn mint(&self, user: crate::domain::UserId, ttl_secs: u64, now_unix: u64) -> anyhow::Result<String>; // RS256 JWT
    pub fn verify(&self, token: &str, now_unix: u64) -> Option<AccessClaims>; // sig+exp+iss+aud(="soul-player"); None on any failure
    pub fn jwks(&self) -> serde_json::Value; // { "keys": [ { kty:"RSA", use:"sig", alg:"RS256", kid, n, e } ] }
}
```
Deps to add: `jsonwebtoken = "9"`, `rsa = "0.9"`, `base64 = "0.22"` (for JWK n/e), `sha2 = "0.10"` (used in B). Keep `rand`.

- [ ] **Step 1 — failing test** (`signer.rs`): `load_or_create(tmp, "http://h".into())`; `mint(7, 3600, now)` → a token; `verify(token, now)` → `Some(claims)` with `sub=="7"`, `aud=="soul-player"`, `iss=="http://h"`, `scope=="library"`; `verify(token, now + 4000)` (past exp) → `None`; `verify("garbage", now)` → `None`; a second `load_or_create` on the same dir reuses the key so a token minted by the first still verifies under the second (persistence); `jwks()["keys"][0]["kid"]` is non-empty and `["alg"]=="RS256"`.
- [ ] **Step 2 — run, expect FAIL** (`cargo test -p soul-server signer`). **Step 3 — implement** against the `jsonwebtoken`/`rsa` APIs (generate RSA-2048, write/read PKCS#8 PEM with 0600 perms where supported; `EncodingKey::from_rsa_pem`/`DecodingKey::from_rsa_pem`; `Validation` with `set_audience(&["soul-player"])` + `set_issuer(&[issuer])` + `validate_exp`; JWK n/e = base64url of the modulus/exponent; `kid` = a hash of the public key). **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit** `feat(server): RS256 Signer — persisted keypair, JWT mint/verify, JWKS`.

### Task A2: discovery + `/jwks` routes

**Files:** Modify `crates/soul-server/src/proto/native.rs` (routes + handlers), `crates/soul-server/src/domain/service.rs` or a new field so the router can reach the `Signer`.

**Interfaces — Consumes:** `Signer` (A1). **Produces:** `GET /.well-known/openid-configuration` + `GET /jwks` on the router. The `Services`/`AppState` gains the `Signer` + the server's `issuer` (base URL) + token TTL, so handlers can reach them. (Add `pub signer: Arc<Signer>` + `pub issuer: String` to the app state wiring; `router(...)` signature grows accordingly — later tasks + the E2E must pass them.)

- [ ] **Step 1 — failing http test** (`tests/http.rs`, `oneshot`): `GET /.well-known/openid-configuration` → 200 JSON with `issuer`, `authorization_endpoint` ending `/authorize`, `token_endpoint` ending `/token`, `jwks_uri` ending `/jwks`, `revocation_endpoint` ending `/revoke`, `response_types_supported:["code"]`, `grant_types_supported` containing `"authorization_code"` and `"refresh_token"`, `code_challenge_methods_supported:["S256"]`. `GET /jwks` → 200 with a `keys` array whose first key has `kty:"RSA"`, `alg:"RS256"`, a `kid`. Both unauthenticated.
- [ ] **Step 2 — FAIL; implement the two handlers + thread the `Signer`/issuer into the router; PASS.**
- [ ] **Step 3 — commit** `feat(server): OIDC discovery + JWKS endpoints`.

---

## Increment B — refresh-token store

### Task B1: `RefreshStore` — durable, hashed, rotating, revocable

**Files:** Create `crates/soul-server/src/auth/refresh.rs`; Modify `crates/soul-server/src/store.rs` (persist refresh records alongside users) or a sibling file next to the users JSON.

**Interfaces — Produces:**
```rust
pub struct RefreshRecord { pub token_sha256: String, pub user_id: crate::domain::UserId, pub created_at: u64 } // serde
pub trait RefreshStore: Send + Sync {
    fn issue(&self, user: crate::domain::UserId, now: u64) -> anyhow::Result<String>; // returns the RAW token; stores only its sha256
    fn consume_rotate(&self, raw: &str, now: u64) -> Option<(crate::domain::UserId, String)>; // verify+DELETE old, issue+return new raw; None if unknown
    fn revoke(&self, raw: &str) -> bool; // delete by sha256; true if existed
}
```
Implement on `FileUsers` (it already owns the JSON file + Mutex): add a `refresh: Vec<RefreshRecord>` to the persisted structure and `impl RefreshStore for FileUsers`. Hash with SHA-256 (hex). Persist atomically (reuse the existing temp-file+rename write). Rotation = `consume_rotate` deletes the presented token and issues a fresh one in one locked step.

- [ ] **Step 1 — failing test** (`refresh.rs` or `store.rs`): open a `FileUsers` in a temp dir; `issue(5, now)` → raw token `t1`; `consume_rotate(t1, now)` → `Some((5, t2))` with `t2 != t1`; a second `consume_rotate(t1, now)` → `None` (old token invalid after rotation); `consume_rotate(t2, now)` → `Some((5, t3))`; `revoke(t3)` → `true`, then `consume_rotate(t3, now)` → `None`; reopening `FileUsers` on the same dir still rotates a surviving token (persistence); the file never stores a raw token (grep the file bytes for `t1` → absent).
- [ ] **Step 2 — FAIL; implement; PASS** (`cargo test -p soul-server refresh`).
- [ ] **Step 3 — commit** `feat(server): durable, hashed, rotating refresh-token store`.

---

## Increment C — Authorization Server endpoints

### Task C1: auth-code store + PKCE + grant logic (pure)

**Files:** Create `crates/soul-server/src/auth/oauth.rs`.

**Interfaces — Produces:**
```rust
pub struct AuthCode { pub code: String, pub user: crate::domain::UserId, pub code_challenge: String, pub redirect_uri: String, pub expires_at: u64 }
pub struct CodeStore { /* Mutex<Vec<AuthCode>> */ }
impl CodeStore {
    pub fn new() -> Self;
    pub fn issue(&self, user, code_challenge: String, redirect_uri: String, now: u64, ttl: u64) -> String; // random code
    pub fn consume(&self, code: &str, code_verifier: &str, redirect_uri: &str, now: u64) -> Option<crate::domain::UserId>; // single-use: verify S256(verifier)==challenge, redirect matches, unexpired; DELETE
}
pub fn verify_pkce_s256(code_verifier: &str, code_challenge: &str) -> bool; // base64url(sha256(verifier)) == challenge
pub fn is_loopback_redirect(uri: &str) -> bool; // http://127.0.0.1:{port}/callback or [::1]; any port; no other host
```

- [ ] **Step 1 — failing test**: `verify_pkce_s256` with a known RFC 7636 verifier/challenge pair (use the appendix B test vector) → true; a wrong verifier → false. `is_loopback_redirect("http://127.0.0.1:49152/callback")` → true; `"http://evil.com/callback"` / `"http://localhost/callback"` (per RFC 8252, prefer `127.0.0.1` literal) → false. `CodeStore`: `issue` then `consume` with the right verifier+redirect → `Some(user)`; a second `consume` of the same code → `None` (single-use); `consume` with a wrong verifier → `None`; an expired code → `None`.
- [ ] **Step 2 — FAIL; implement; PASS** (`cargo test -p soul-server oauth`).
- [ ] **Step 3 — commit** `feat(server): auth-code store + PKCE S256 + loopback redirect validation`.

### Task C2: `/authorize`, `/token`, `/revoke` HTTP endpoints

**Files:** Modify `crates/soul-server/src/proto/native.rs` (routes + handlers), add `askama`/plain-string HTML for the login page (use a plain `format!` HTML string — no template engine dep).

**Interfaces — Consumes:** `Signer` (A1), `RefreshStore` (B1), `CodeStore`+PKCE (C1), `FileUsers::verify` (existing). **Produces:** the three endpoints on the router.
- `GET /authorize` — validate params (`response_type=code`, `client_id=soul-player`, loopback `redirect_uri`, `code_challenge`, `code_challenge_method=S256`, `state`); render a minimal HTML login form that POSTs `username`,`password`,and the echoed oauth params back to `/authorize`.
- `POST /authorize` (form) — `FileUsers::verify`; on success `CodeStore::issue` and `302 Location: {redirect_uri}?code=...&state=...`; on failure re-render the form with an error (still 200).
- `POST /token` (form) — `authorization_code` grant: `CodeStore::consume` → `Signer::mint` access + `RefreshStore::issue` → JSON `{access_token, token_type:"Bearer", expires_in, refresh_token}`. `refresh_token` grant: `RefreshStore::consume_rotate` → new access + rotated refresh. Errors → `400 {"error":"invalid_grant"}` etc. (OAuth error JSON).
- `POST /revoke` (form) — `RefreshStore::revoke(token)`; always `200` (RFC 7009).

- [ ] **Step 1 — failing http test** (`tests/http.rs`) driving the WHOLE code flow with `oneshot`:
  1. `GET /authorize?response_type=code&client_id=soul-player&redirect_uri=http://127.0.0.1:12345/callback&code_challenge=<S256(v)>&code_challenge_method=S256&state=xyz&scope=library` → 200 HTML containing a password field.
  2. `POST /authorize` with `username=u&password=p` + the echoed params → `302` whose `Location` is `http://127.0.0.1:12345/callback?code=<C>&state=xyz`.
  3. `POST /token` `grant_type=authorization_code&code=<C>&code_verifier=<v>&redirect_uri=...&client_id=soul-player` → 200 `{access_token, refresh_token, ...}`; the `access_token` verifies via the server `Signer`.
  4. `POST /token` `grant_type=refresh_token&refresh_token=<R>` → 200 with a NEW access + NEW refresh; replaying the old refresh → 400.
  5. `POST /authorize` with a wrong password → 200 (form again), no `302`/code.
  6. `POST /token` with a tampered `code_verifier` → 400.
  7. `POST /revoke` with the refresh → 200, then refresh grant with it → 400.
- [ ] **Step 2 — FAIL; implement; PASS.**
- [ ] **Step 3 — commit** `feat(server): OAuth2 /authorize (PKCE login), /token (code+refresh), /revoke`.

---

## Increment D — migrate `/v0/*` to JWT + remove the old scheme

### Task D1: `Authed` validates the JWT; delete `/v0/auth` + opaque tokens

**Files:** Modify `crates/soul-server/src/proto/native.rs` (`Authed`, remove `/v0/auth` + `auth` handler + `AuthRequest`), `crates/soul-server/src/domain/service.rs` (remove `authenticate`/`check`; keep `index`/`open_track`), `crates/soul-server/src/store.rs` (remove `issue_token`/`check` opaque methods + the `UserStore` trait methods for them; keep `verify`/`add_user`), `crates/soul-server/src/domain/ports.rs` (drop `issue_token`/`check` from `UserStore`).

**Interfaces:** `Authed` now = `bearer JWT → Signer::verify → UserId from sub`.

- [ ] **Step 1 — update the existing http tests** that used `/v0/auth`/opaque tokens: obtain a JWT via the Task-C2 flow (a small test helper `fn login(router,…) -> access_token` driving authorize+token), then hit `/v0/index`/`/v0/track` with `Authorization: Bearer <jwt>`. Assert: a valid JWT → 200; no token → 401; an expired/garbage/wrong-signature JWT → 401.
- [ ] **Step 2 — run (FAIL where old `/v0/auth` is referenced); implement the `Authed` JWT path + delete the opaque scheme; PASS.** Confirm `cargo build -p soul-server` + `cargo test -p soul-server` green and clippy clean.
- [ ] **Step 3 — commit** `feat(server): /v0 validates JWT access tokens; drop the opaque /v0/auth scheme`.

---

## Increment E — client Relying Party

### Task E1: `TokenStore` trait + `KeyringStore` + `MemStore`

**Files:** Modify `crates/soul-player-host/src/remote.rs` (add the trait + impls); `crates/soul-player-host/Cargo.toml` (`keyring = "3"`).

**Interfaces — Produces:**
```rust
pub trait TokenStore: Send + Sync {
    fn save_refresh(&self, server_url: &str, refresh: &str) -> anyhow::Result<()>;
    fn load_refresh(&self, server_url: &str) -> Option<String>;
    fn delete_refresh(&self, server_url: &str) -> anyhow::Result<()>;
}
pub struct KeyringStore;  // keyring crate: service "soul-player", account = server_url
pub struct MemStore(std::sync::Mutex<std::collections::HashMap<String,String>>); // tests
```
- [ ] **Step 1 — failing test**: `MemStore` round-trips save/load/delete per server_url; `load_refresh` of an unknown url → `None`.
- [ ] **Step 2 — FAIL; implement both (`KeyringStore` via `keyring::Entry::new("soul-player", url)` set/get/delete, mapping `NoEntry` → `None`); PASS** (`cargo test --manifest-path crates/soul-player-host/Cargo.toml token_store`).
- [ ] **Step 3 — commit** `feat(host): TokenStore (keyring + in-memory)`.

### Task E2: `login` (discovery + loopback + PKCE + browser) and `refresh`

**Files:** Modify `crates/soul-player-host/src/remote.rs`; `crates/soul-player-host/Cargo.toml` (`oauth2 = "5"`, `openidconnect = "4"`, `open = "5"`, and `tiny_http`/`std::net` for the loopback catcher — prefer a hand-rolled `std::net::TcpListener` one-shot reader to avoid a dep).

**Interfaces — Produces:**
```rust
pub struct Tokens { pub access: String, pub refresh: String }
pub fn login(url: &str) -> anyhow::Result<Tokens>;            // full interactive browser flow
pub fn refresh(url: &str, refresh: &str) -> anyhow::Result<Tokens>;
// testable seam:
pub fn exchange_code(url: &str, code: &str, verifier: &str, redirect_uri: &str) -> anyhow::Result<Tokens>; // POST /token
fn authorize_url(url, challenge, state, redirect_uri) -> String;  // build /authorize URL (discovery for endpoints)
```
`login` = discover → bind `TcpListener` 127.0.0.1:0 → build authorize URL (PKCE verifier/challenge + state) → `open::that(authorize_url)` → accept one connection, parse `GET /callback?code=..&state=..`, verify `state`, write a tiny 200 "You can close this tab" → `exchange_code`. The browser-open + socket-accept live in `login`; `exchange_code`/`refresh`/`authorize_url` are independently testable.

- [ ] **Step 1 — failing test** against a real in-process AS (reuse the http-test harness that stands up `router`): build a valid PKCE pair, drive `GET/POST /authorize` via a blocking `reqwest` client to get a `code` (simulating the browser), then assert `exchange_code(url, code, verifier, redirect)` → `Tokens` whose `access` is accepted by `GET /v0/index`; then `refresh(url, tokens.refresh)` → new `Tokens` whose access also works and whose refresh differs. (Start the AS on a real ephemeral `tokio` listener as `remote_e2e`/the existing remote tests already do.)
- [ ] **Step 2 — FAIL; implement `exchange_code`/`refresh`/`authorize_url` (via `oauth2`/`openidconnect` or hand-rolled form POSTs — either is fine; keep `login` thin around them); PASS.** (Do not unit-test the `open::that` browser launch.)
- [ ] **Step 3 — commit** `feat(host): OAuth2 Authorization-Code+PKCE login + refresh`.

### Task E3: wire the new auth into `RemoteServer`/`add_remote_source`

**Files:** Modify `crates/soul-player-host/src/remote.rs` (`RemoteServer`, `fetch_index`, `DownloadCache` use the access token — already `Bearer`), `crates/soul-player-host/src/lib.rs` (`add_remote_source`). Remove `remote::authenticate`.

**Interfaces:** `RemoteServer { url, access_token }` (the `token` field is now the JWT access token). `PlayerHandle::add_remote_source(&self, url: &str) -> anyhow::Result<()>` now: `login(url)` (blocks on the browser round-trip) → store refresh via a `TokenStore` → build `RemoteServer{url, access_token}` → fetch_index + rebuild (unchanged downstream). Signature changes from `(url,user,pass)` to `(url)`. For tests/E2E (no browser), also expose `pub fn add_remote_source_with_tokens(&self, url: &str, tokens: Tokens) -> anyhow::Result<()>`.

- [ ] **Step 1 — failing test**: host test using `add_remote_source_with_tokens(url, tokens)` (tokens from the Task-E2 flow against an in-process AS) → `views()` non-empty (the authed fetch_index worked with the JWT).
- [ ] **Step 2 — FAIL; implement; remove `remote::authenticate`; PASS.** Keep the whole host suite green.
- [ ] **Step 3 — commit** `feat(host): wire JWT access token into the remote source`.

### Task E4: rune connect form → browser-login model (off-thread)

**Files:** Modify `app/rune/src/views/settings.rs` (the remote connect form), `app/rune/src/main.rs` (`connect_remote_source` + the `remote_*_buf` state), `app/rune/locales/app.yml`. (`app/rune` is its OWN nightly workspace — `cd app/rune && cargo test --bin soul-player-rune`.)

**Why:** `add_remote_source` is now `(url)` and the password is entered in the **system browser**, not the app — so the connect form drops the username/password fields, keeps only the **server URL** + a **Connect** button whose label makes clear it opens the browser, and runs the (blocking) login **off-thread** (the existing `scan_in_background` + `AppHandle::update` pattern) so the UI never freezes. On success close the form + refresh the library; on error show the message; while connecting show a "waiting for browser…" state.

**Interfaces — Consumes:** `PlayerHandle::add_remote_source(url)` (E3). 

- [ ] **Step 1 — update the stale test**: the existing `remote_add_source_opens_the_connect_form` test asserts `remote-user`/`remote-pass` fields — change it to assert the form now has `remote-url` + `remote-connect` (focusable + labelled), and that `remote-user`/`remote-pass` are GONE. Write it to fail first against the current (password) form.
- [ ] **Step 2 — implement**: drop `remote_user_buf`/`remote_pass_buf` (keep `remote_url_buf`), make `connect_remote_source` spawn a background thread that calls `s.host.add_remote_source(&url)` and reports back via `AppHandle::update` (mirror `scan_in_background`); localize the new/changed strings across every `app.yml` locale; keep every control keyed + `.label`'d. Run `cargo test --bin soul-player-rune` (from `app/rune`) — the updated test + the full rune suite green.
- [ ] **Step 3 — commit** `feat(ui): remote connect form uses the browser login (off-thread)`.

---

## Increment F — migrate the E2E + verify

### Task F1: migrate `remote_e2e` to the standards-based login + full verification

**Files:** Modify `crates/soul-player-host/tests/remote_e2e.rs`.

- [ ] **Step 1 — rewrite the E2E auth step**: stand up the AS (as now), seed a user via `FileUsers::add_user`, drive authorize+token over HTTP (simulating the browser, as in E2) to get `Tokens`, call `add_remote_source_with_tokens(url, tokens)`, then keep the existing assertions: `views()` shows the server's tracks (Aria / Open Goldberg Variations) and a remote track decodes. Run `cargo test --manifest-path crates/soul-player-host/Cargo.toml --test remote_e2e` → green.
- [ ] **Step 2 — full verification across all THREE workspaces**: `cargo fmt --all` + `cargo clippy --all-targets -- -D warnings` clean on root, host, AND `app/rune` (run from `app/rune`); `cargo test` (root) + `cargo test --manifest-path crates/soul-player-host/Cargo.toml` + `cargo test --bin soul-player-rune` (from `app/rune`) all green; **`cargo audit` clean** (the new deps introduce no advisory — if one fires, bump/patch like the rusqlite→flat-file fix) and `cargo deny check` passes (licenses of `jsonwebtoken`/`oauth2`/`openidconnect`/`keyring`/`open`/`rsa`). Merge latest `main`; re-verify. Ready for PR.
- [ ] **Step 3 — commit** `test(auth): migrate remote E2E to the standards-based login`.
