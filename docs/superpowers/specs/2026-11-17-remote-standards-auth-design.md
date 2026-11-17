# Remote auth — standards-based (OAuth 2.1 / OIDC) — design

Program: Remote-source auth rework. Supersedes the bespoke `POST /v0/auth {user,pass}` → opaque bearer token shipped in PR #208.
Date: 2026-11-17
Branch: `feat/remote-standards-auth`
Research: `.superpowers/sdd/remote-oidc-auth-research.md` (+ `remote-login-persistence-research.md`).

## Goal

Replace the custom username/password-in-the-client auth with a **standards-compliant OAuth 2.1 flow**: the desktop client logs in via the **system browser** using **Authorization Code + PKCE** (RFC 8252 + RFC 7636), and `soul-server` acts as a minimal **Authorization Server + Resource Server** issuing short-lived **JWT access tokens** (RFC 9068) and **rotating refresh tokens**, published via **OIDC Discovery + JWKS**. The client never sees the password; the refresh token lives in the **OS keychain**. `/v0/*` validates the JWT locally.

This is foundational **sub-project 1**. **Sub-project 2** (remote-source persistence, Settings → Sources rows, remove/revoke, offline-vs-reconnect UX, off-thread) builds on the refresh-token/keychain layer this delivers and gets its own spec.

## Architecture

```
  Desktop client (RP / public client)                soul-server (AS + RS)
  "Add remote":                                       GET  /.well-known/openid-configuration  (discovery)
   1. discover (/.well-known/openid-configuration)    GET  /jwks                               (public keys)
   2. start loopback listener 127.0.0.1:{ephemeral}   GET  /authorize  -> HTML login (Argon2id) -> 302 code
   3. open SYSTEM BROWSER -> /authorize?...PKCE...     POST /token      -> code+verifier | refresh -> {access(JWT), refresh}
   4. receive code on loopback redirect               POST /revoke     -> invalidate a refresh token (RFC 7009)
   5. POST /token (code + code_verifier) -> tokens     /v0/*            -> validate the JWT access token (sig+exp+iss+aud)
   6. refresh token -> OS keychain; {url,user} -> cfg
```

Client libs: `oauth2` (PKCE + the code/refresh exchanges) + `openidconnect` (discovery/JWKS/RP) + `keyring` (secret storage). Server: hand-rolled on `jsonwebtoken` + `axum` (no mature axum AS framework; the surface is small).

## Server — Authorization Server

### Endpoints
- `GET /.well-known/openid-configuration` — JSON discovery: `issuer` (the server's base URL), `authorization_endpoint`, `token_endpoint`, `revocation_endpoint`, `jwks_uri`, `response_types_supported:["code"]`, `grant_types_supported:["authorization_code","refresh_token"]`, `code_challenge_methods_supported:["S256"]`, `token_endpoint_auth_methods_supported:["none"]` (public client). Unauthenticated.
- `GET /jwks` — the signing public key(s) as a JWK Set. Unauthenticated.
- `GET /authorize` — params `response_type=code`, `client_id=soul-player`, `redirect_uri` (must be a `127.0.0.1:{port}` loopback — validated per RFC 8252 §7.3: allow any port on 127.0.0.1/[::1]), `code_challenge`, `code_challenge_method=S256`, `state`, `scope`. Renders a **minimal HTML login page** (username + password). On submit (`POST /authorize` or `/login`), Argon2id-verify; on success mint a single-use **authorization code** (bound to the PKCE `code_challenge`, the `redirect_uri`, and the user; short TTL ~60 s) and `302` to `redirect_uri?code=...&state=...`.
- `POST /token` — `application/x-www-form-urlencoded`:
  - `grant_type=authorization_code` + `code` + `code_verifier` + `redirect_uri` + `client_id` → verify the code (unexpired, unused, redirect_uri matches, `S256(code_verifier)==code_challenge`), then issue `{access_token (JWT), token_type:"Bearer", expires_in, refresh_token}`.
  - `grant_type=refresh_token` + `refresh_token` → verify + **rotate** (invalidate the old, issue a new refresh) and return a fresh access token. (OAuth 2.1 refresh rotation.)
- `POST /revoke` (RFC 7009) — `token` (+ optional `token_type_hint`) → invalidate the refresh token; always `200`.

### Tokens
- **Access = JWT** (RFC 9068), signed **RS256** (a 2048-bit RSA keypair generated on first run, persisted to the server dir so tokens + JWKS survive restart). Claims: `iss`, `sub` (user id), `aud` ("soul-player"), `exp` (TTL ~1 h), `iat`, `scope`. `/v0/*` verifies signature against the in-memory public key + checks `exp`/`iss`/`aud` — no network, no introspection (single process).
- **Refresh = opaque**, 32 CSPRNG bytes; **persisted durably** in the user store (a `refresh_tokens` section alongside users: `{token_hash, user_id, created_at}` — store a hash, not the raw token). Rotated on use; revocable. (This replaces the ephemeral in-memory tokens from PR #208.)
- **Auth codes** live in-memory (short TTL) — fine to lose on restart.

### `/v0/*` change
The `Authed` extractor (`proto/native.rs`) stops calling `state.check(opaque)` and instead **validates the `Bearer` JWT** (verify sig + claims) → `UserId` from `sub`. Remove the old `POST /v0/auth` route, `Services::authenticate`, and `FileUsers::issue_token`/`check` opaque-token methods (replaced by the AS token endpoints + the refresh store). Keep `verify`/`add_user` (used by `/authorize` + the CLI).

### Domain / hexagon fit
A new `Auth` module (the AS) composes `UserStore` (verify) + a `RefreshStore` (persist/rotate/revoke refresh tokens) + a `Signer` (RS256 sign/verify, JWKS). These are ports/adapters like the rest. The protocol adapter (`proto/native.rs`) adds the AS routes. `AuthMode::ExternalOidc` (validate an external IdP's JWTs via its discovery/JWKS instead of our own) is **deferred** — the `/v0/*` JWT-validation path is written so it can later accept a configured external issuer with no client change.

## Client — Relying Party

- `remote::authenticate(url,user,pass)` is **replaced** by `remote::login(url) -> anyhow::Result<Tokens>`:
  1. Discover (`/.well-known/openid-configuration`).
  2. Bind a loopback `TcpListener` on `127.0.0.1:0`; `redirect_uri = http://127.0.0.1:{port}/callback`.
  3. Build the `/authorize` URL with a fresh PKCE `code_verifier`/`code_challenge` + `state`; **open the system browser** (`open`/`webbrowser` crate).
  4. Wait for the browser redirect to the loopback, read `code` (+ verify `state`); serve a tiny "you can close this tab" page.
  5. Exchange at `/token` (code + verifier) → `{access_token, refresh_token}`.
- `RemoteServer` becomes `{ url, access_token, user }` with the refresh token held in the keychain; a `fn bearer(&self) -> access_token` used by `fetch_index`/`DownloadCache` (unchanged otherwise — still `Authorization: Bearer <jwt>`).
- `fn refresh(url, refresh_token) -> Tokens` for silent re-auth when the access token is near/after expiry.
- **Token storage** behind a `TokenStore` trait: `KeyringStore` (the `keyring` crate — Keychain / Credential Manager / Secret Service) in the app; an in-memory impl for tests. Refresh token → keychain keyed by the server URL; `{url,user}` → app config.
- The browser-launch + loopback steps are factored so the **token-exchange + refresh logic is unit-testable** by driving HTTP against a real in-process AS (the browser step is the only untested seam; inject the code in tests).

## Migration of existing code + tests

- `proto/native.rs` http tests, `remote.rs` tests, and `tests/remote_e2e.rs` currently call `/v0/auth`/`authenticate` → rewrite to drive the new flow at the HTTP level: POST the `/authorize` login form → follow the `302` → extract `code` → `POST /token` → use the JWT. (No real browser in tests.)
- The `remote_e2e` acceptance test keeps proving **add → sync → browse → play**, now through the standards-based login.

## Scope (YAGNI)

**In:** discovery, JWKS, `/authorize` (minimal HTML login), `/token` (auth-code+PKCE and refresh with rotation), `/revoke`, RS256 JWT access tokens, durable+rotating refresh tokens, `/v0/*` JWT validation, the client loopback+PKCE+browser login + refresh + keychain storage, migrated tests + E2E.

**Out (follow-ups):** external-IdP delegation (`AuthMode::ExternalOidc`); DPoP, PAR, token introspection; multi-client / dynamic client registration; a styled login page / "remember me" browser session; scopes beyond a single `library` scope; **sub-project 2** (persistence of remotes, Sources rows, remove/revoke UI, offline/reconnect UX, off-thread connect).

## Testing strategy

- **Server**: discovery + JWKS shape; `/authorize` GET renders the form and rejects a non-loopback `redirect_uri`; the full code flow (login form POST → code → `/token` → JWT) succeeds and a tampered PKCE verifier fails; refresh rotates (old refresh then rejected); revoke invalidates; `/v0/index` accepts a valid JWT and rejects a missing/expired/wrong-signature one.
- **Client**: `TokenStore` in-memory round-trip; `login` against a real in-process AS with the browser step stubbed (inject the authorization code); `refresh` yields a new usable access token; an expired access token triggers a refresh.
- **E2E**: `remote_e2e` — standards-based login → add → sync → browse → play.

## Risks

- **Server-side AS is security-sensitive** — PKCE S256 verification, single-use codes, redirect_uri loopback validation, refresh rotation/revocation, JWT `aud`/`iss`/`exp` checks must all be correct; each gets a dedicated test + review.
- **`keyring` needs an OS secret backend** — headless CI/Linux may lack Secret Service; gate it behind the `TokenStore` trait (in-memory for tests) and document the Linux requirement (or a config-file fallback flagged as less-secure).
- **RSA keygen on first run** adds a little startup cost (one-time) + a key file in the server dir (persist with tight perms).
- **3-workspace churn**: server (root), host (client) — same merge discipline as PR #208; new deps (`jsonwebtoken`, `oauth2`, `openidconnect`, `keyring`, `open`) all pure-Rust / cross-platform (watch `cargo audit`/`deny` like last time).
