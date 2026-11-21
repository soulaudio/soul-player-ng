# Remote Source Server — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A self-hostable `soul-server` that serves a music library over a native bit-perfect protocol, and `soul-player-ng` mounting it as a Remote source — proven by an add→sync→browse→play E2E test.

**Architecture:** Hexagonal `soul-server` (one crate, modules per port/adapter): a protocol-agnostic domain (ports = traits) with a native HTTP protocol adapter, a `soul-audio-core`-backed index adapter, a filesystem media adapter, and a SQLite user store. The `-ng` client adds a Remote source that syncs the server's `.soulidx` into the existing `Library::from_source_bytes` aggregation and plays tracks by downloading originals to a cache and reusing the existing `open_track` path.

**Tech Stack:** Rust (root workspace: edition 2024, rust 1.85); `axum`/`tokio` (server HTTP), `rusqlite` (user store), `argon2` (password hash), `reqwest` (blocking, client fetch), `serde`/`serde_json`, `soul-audio-core` (scan/index, already a git dep of the backend/host).

## Global Constraints

- Root workspace is edition 2024, rust-version 1.85. `soul-server` is a new member of the ROOT workspace (`crates/soul-server`). `app/rune` stays its own nightly workspace; `crates/soul-player-host` stays its own workspace.
- Commit messages: conventional; NEVER reference Claude/Anthropic/AI/co-authors (a commit-msg hook rejects those words).
- `cargo fmt --all` + `cargo clippy --all-targets -- -D warnings` clean before each commit. Run server/host cargo per their workspace (`crates/soul-player-host` is its own workspace: `cargo test --manifest-path crates/soul-player-host/Cargo.toml`).
- Native protocol version prefix is `/v0`. All non-`ping`/`auth` routes require `Authorization: Bearer <token>`. Media is served untouched (no transcode); `/v0/track` must support HTTP `Range`.
- TDD: write the failing test, watch it fail, minimal code to pass, commit. Each task ends green.
- Reuse, don't reinvent: the backend already has `Library::scan_many(roots)`, `Library::from_source_bytes(roots, bytes)`, `Library::path_of(id) -> Option<PathBuf>`, `Library::source_counts()`. `.soulidx` is the baked per-root index the host already round-trips.

---

## File structure

- `crates/soul-server/Cargo.toml` — new crate (bin + lib), root workspace member.
- `crates/soul-server/src/lib.rs` — re-exports; wires the hexagon.
- `crates/soul-server/src/domain/mod.rs` — `Track`, `IndexVersion`, `UserId`, `Token`, errors.
- `crates/soul-server/src/domain/ports.rs` — the port traits: `IndexPort`, `MediaStore`, `UserStore`.
- `crates/soul-server/src/domain/service.rs` — use cases: `Catalog`, `Auth`, `Streaming` composing the ports.
- `crates/soul-server/src/index.rs` — `CoreIndex: IndexPort` over `soul-audio-core` scan (reuses the backend's `Library`).
- `crates/soul-server/src/media.rs` — `FsMedia: MediaStore` (open `Read+Seek`, len, content-type).
- `crates/soul-server/src/store.rs` — `SqliteUsers: UserStore` (Argon2id, tokens).
- `crates/soul-server/src/proto/native.rs` — axum router: `/v0/*`.
- `crates/soul-server/src/config.rs` — `soul-server.toml` load.
- `crates/soul-server/src/bin/soul-server.rs` — CLI (`serve`, `user add`).
- `crates/soul-player-backend/src/library.rs` — ADD `Library::locate(id) -> Option<Located>`.
- `crates/soul-player-host/src/remote.rs` — NEW: `RemoteSource`, `MediaFetch`, `TrackLocation`, index sync (client of `/v0`).
- `crates/soul-player-host/src/lib.rs` — wire remote sources into the aggregation + loader.
- `crates/soul-player-host/tests/remote_e2e.rs` — the E2E test (path dev-dep on `soul-server`).
- `app/rune/src/views/settings.rs` + `app/rune/src/main.rs` — enable the Remote add-source item + connect form.

---

## Increment A — server skeleton, domain ports, SQLite users, CLI

### Task A1: Create the `soul-server` crate in the root workspace

**Files:** Create `crates/soul-server/Cargo.toml`, `crates/soul-server/src/lib.rs`; Modify `Cargo.toml` (root `[workspace] members`).

- [ ] **Step 1 — add the crate.** `Cargo.toml` (root): add `"crates/soul-server"` to `members`. Create `crates/soul-server/Cargo.toml`:
```toml
[package]
name = "soul-server"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "Apache-2.0"

[dependencies]
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[lib]
path = "src/lib.rs"
```
Create `src/lib.rs` with `pub mod domain;` and a `pub fn version() -> &'static str { env!("CARGO_PKG_VERSION") }`.
- [ ] **Step 2 — placeholder module.** Create `src/domain/mod.rs` empty (`// filled in A2`). `cargo build -p soul-server` compiles.
- [ ] **Step 3 — test it builds + version.** Add `#[cfg(test)] mod tests { #[test] fn has_version() { assert!(!crate::version().is_empty()); } }` to `lib.rs`. Run `cargo test -p soul-server`. Expected PASS.
- [ ] **Step 4 — commit.** `git add crates/soul-server Cargo.toml && git commit -m "feat(server): soul-server crate skeleton"`.

### Task A2: Domain types + port traits

**Files:** `crates/soul-server/src/domain/mod.rs`, `crates/soul-server/src/domain/ports.rs`.

**Interfaces — Produces** (used by every later server task):
```rust
// domain/mod.rs
pub type UserId = i64;
#[derive(Clone, Debug, PartialEq, Eq)] pub struct Token(pub String);
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct IndexVersion(pub u64); // monotonic; also the ETag
#[derive(thiserror::Error, Debug)] pub enum DomainError {
    #[error("unauthorized")] Unauthorized,
    #[error("not found")] NotFound,
    #[error(transparent)] Other(#[from] anyhow::Error),
}
// domain/ports.rs
pub trait IndexPort: Send + Sync {
    fn version(&self) -> IndexVersion;
    fn index_bytes(&self) -> anyhow::Result<Vec<u8>>;               // the aggregated .soulidx
    fn locate(&self, track_id: u32) -> Option<std::path::PathBuf>;  // absolute path on the server
}
pub trait MediaStore: Send + Sync {
    fn open(&self, path: &std::path::Path) -> std::io::Result<Box<dyn ReadSeek>>;
    fn len(&self, path: &std::path::Path) -> std::io::Result<u64>;
}
pub trait ReadSeek: std::io::Read + std::io::Seek + Send {}
impl<T: std::io::Read + std::io::Seek + Send> ReadSeek for T {}
pub trait UserStore: Send + Sync {
    fn verify(&self, user: &str, pass: &str) -> Option<UserId>;
    fn issue_token(&self, user: UserId) -> anyhow::Result<Token>;
    fn check(&self, token: &str) -> Option<UserId>;
    fn add_user(&self, user: &str, pass: &str) -> anyhow::Result<UserId>;
}
```
- [ ] **Step 1 — add `thiserror` dep** to `Cargo.toml` (`thiserror = "2"`).
- [ ] **Step 2 — write the types + traits** verbatim above into the two files; `pub mod ports;` from `domain/mod.rs`.
- [ ] **Step 3 — a trait-object smoke test.** In `domain/mod.rs` tests: assert the traits are object-safe:
```rust
#[test] fn ports_are_object_safe() {
    fn _a(_: &dyn ports::UserStore) {} fn _b(_: &dyn ports::IndexPort) {} fn _c(_: &dyn ports::MediaStore) {}
}
```
Run `cargo test -p soul-server`. Expected PASS (compiles = object-safe).
- [ ] **Step 4 — commit.** `feat(server): domain types + hexagon port traits`.

### Task A3: SQLite `UserStore` with Argon2id + tokens

**Files:** `crates/soul-server/src/store.rs`; Modify `Cargo.toml` (`rusqlite = { version = "0.32", features = ["bundled"] }`, `argon2 = "0.5"`, `rand = "0.8"`), `src/lib.rs` (`pub mod store;`).

**Interfaces — Produces:** `pub struct SqliteUsers { .. }` with `pub fn open(path: &Path) -> anyhow::Result<Self>` (creates the schema if absent) and `impl UserStore for SqliteUsers`. Schema: `users(id INTEGER PRIMARY KEY, name TEXT UNIQUE, pass_hash TEXT, subsonic_secret TEXT NULL)`, `tokens(token TEXT PRIMARY KEY, user_id INTEGER, created_at INTEGER)`. Passwords hashed with Argon2id (`argon2::Argon2::default().hash_password`). Tokens are 32-byte URL-safe random (`rand`), stored verbatim; `check` looks them up.

- [ ] **Step 1 — failing test** in `store.rs`:
```rust
#[test] fn add_verify_token_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let s = SqliteUsers::open(&dir.path().join("u.db")).unwrap();
    let id = s.add_user("alice", "hunter2").unwrap();
    assert_eq!(s.verify("alice", "hunter2"), Some(id));
    assert_eq!(s.verify("alice", "wrong"), None);
    assert_eq!(s.verify("nobody", "x"), None);
    let tok = s.issue_token(id).unwrap();
    assert_eq!(s.check(&tok.0), Some(id));
    assert_eq!(s.check("not-a-token"), None);
}
```
(Add `tempfile = "3"` to `[dev-dependencies]`.)
- [ ] **Step 2 — run, expect FAIL** (`SqliteUsers` undefined). `cargo test -p soul-server add_verify_token_roundtrip`.
- [ ] **Step 3 — implement** `SqliteUsers` against the contract above (open+migrate; add_user hashes with Argon2id; verify re-hashes+compares via `PasswordHash::new` + `verify_password`; issue_token inserts a random token; check selects user_id).
- [ ] **Step 4 — run, expect PASS.**
- [ ] **Step 5 — commit.** `feat(server): SQLite user store with Argon2id + bearer tokens`.

### Task A4: Config + CLI (`serve` scaffold, `user add`)

**Files:** `crates/soul-server/src/config.rs`, `crates/soul-server/src/bin/soul-server.rs`; Modify `Cargo.toml` (`toml = "0.8"`, `clap = { version = "4", features = ["derive"] }`), `src/lib.rs` (`pub mod config;`).

**Interfaces — Produces:** `pub struct Config { pub bind: String, pub roots: Vec<PathBuf>, pub db: PathBuf }` with `pub fn load(path: &Path) -> anyhow::Result<Config>` (TOML). CLI: `soul-server --config <path> serve` and `soul-server --config <path> user add <name>` (prompts for a password via `rpassword`, or `--password` flag for tests).

- [ ] **Step 1 — failing config test** in `config.rs`: write a temp `soul-server.toml` with `bind`, `roots`, `db`; assert `load` parses the three fields.
- [ ] **Step 2 — run, expect FAIL; implement `Config` + `load`; run, expect PASS.**
- [ ] **Step 3 — CLI** in `bin/soul-server.rs`: clap derive with subcommands `Serve` and `User { Add { name: String, #[arg(long)] password: Option<String> } }`. `user add` opens `SqliteUsers::open(cfg.db)` and calls `add_user`. `serve` prints "serve not yet wired" (filled in Task C). Add `clap`, `rpassword = "7"`.
- [ ] **Step 4 — CLI smoke test** (integration test `tests/cli.rs`): run the binary via `assert_cmd` (`assert_cmd = "2"`, dev-dep) `user add bob --password pw` against a temp config/db, then a second invocation verifying via a tiny lib call, OR assert exit 0 + the db file now has the user (open `SqliteUsers` and `verify`). Expected PASS.
- [ ] **Step 5 — commit.** `feat(server): config file + CLI (serve scaffold, user add)`.

---

## Increment B — index (core scan) + media adapters + use cases

### Task B1: `CoreIndex` (IndexPort over the backend `Library`)

**Files:** `crates/soul-server/src/index.rs`; Modify `Cargo.toml` to depend on the backend crate for `Library` (`soul-player-backend = { path = "../soul-player-backend" }`) — OR depend on `soul-audio-core` directly if the backend's `Library` isn't reusable; PREFER `soul-player-backend::Library` since it already bakes `.soulidx` and exposes `path_of`.

**Interfaces — Produces:** `pub struct CoreIndex { lib: ArcSwap<Library>, version: AtomicU64, roots: Vec<PathBuf> }` with `pub fn scan(roots: Vec<PathBuf>) -> anyhow::Result<Self>` (calls `Library::scan_many`), `pub fn rescan(&self) -> anyhow::Result<()>` (bumps version), and `impl IndexPort`. `index_bytes()` returns the aggregated `.soulidx` — reuse however the host serialises it (the backend bakes per-root bytes; concatenating/serialising the aggregate: expose the baked bytes. If `Library` lacks a public "aggregate bytes" accessor, add `Library::index_bytes(&self) -> Vec<u8>` in the backend returning the baked catalog bytes). `locate(id)` delegates to `Library::path_of(id)`.

- [ ] **Step 1 — failing test:** scan `testdata/` (a few known files), assert `version()` is non-zero, `index_bytes()` is non-empty and round-trips (feed it back through `Library::from_source_bytes(&[root], vec![bytes])` → `source_counts`/`len` matches), and `locate(0)` returns a path under the root.
- [ ] **Step 2 — run FAIL; implement; run PASS.** (If a backend accessor is missing, add it in `crates/soul-player-backend/src/library.rs` with its own unit test in the same task.)
- [ ] **Step 3 — commit.** `feat(server): CoreIndex adapter over soul-audio-core scan`.

### Task B2: `FsMedia` (MediaStore over the filesystem) + `Streaming`/`Catalog`/`Auth` use cases

**Files:** `crates/soul-server/src/media.rs`, `crates/soul-server/src/domain/service.rs`.

**Interfaces — Produces:**
- `pub struct FsMedia; impl MediaStore for FsMedia` — `open` returns `Box::new(BufReader::new(File::open(path)?))`; `len` from `metadata`.
- `pub struct Services<I,M,U> { pub index: I, pub media: M, pub users: U }` where `I: IndexPort, M: MediaStore, U: UserStore`, with methods the protocol adapter calls:
  - `authenticate(&self, user, pass) -> Result<Token, DomainError>`
  - `check(&self, token) -> Result<UserId, DomainError>`
  - `index(&self) -> Result<(Vec<u8>, IndexVersion), DomainError>`
  - `open_track(&self, id) -> Result<(Box<dyn ReadSeek>, u64), DomainError>` (locate→open; NotFound if no path)

- [ ] **Step 1 — failing test** (fake ports): a `FakeIndex`/`FakeUsers`/`FakeMedia` in the test; assert `authenticate` wrong-pass → `Unauthorized`; `open_track(unknown)` → `NotFound`; `open_track(known)` returns a reader whose bytes match the fake file.
- [ ] **Step 2 — run FAIL; implement `FsMedia` + `Services`; run PASS.**
- [ ] **Step 3 — commit.** `feat(server): fs media store + Catalog/Auth/Streaming use cases`.

---

## Increment C — native HTTP protocol adapter

### Task C1: axum router `/v0` (ping, auth, index, track Range, art)

**Files:** `crates/soul-server/src/proto/native.rs`; Modify `Cargo.toml` (`axum = "0.7"`, `tokio = { version = "1", features = ["rt-multi-thread","macros","net"] }`, `tower = "0.5"`, `tower-http = { version = "0.6", features = ["fs"] }` for Range), `src/lib.rs` (`pub mod proto { pub mod native; }`), `bin/soul-server.rs` (`serve` now builds `Services` + runs the router).

**Interfaces — Produces:** `pub fn router(services: Arc<Services<CoreIndex, FsMedia, SqliteUsers>>) -> axum::Router` and `pub async fn serve(cfg, services)`. Routes exactly as the spec table: `GET /v0/ping` (json `{server, index_version}`, no auth); `POST /v0/auth` (json in/out); `GET /v0/index` (bytes, `ETag`, honor `If-None-Match` → 304); `GET /v0/track/{id}` (bytes, Range/206 — use `axum_extra`/`tower_http` range handling or `http-range-header`); `GET /v0/art/{album_id}` (bytes or 404). A `Bearer` extractor rejects missing/invalid tokens with 401.

- [ ] **Step 1 — failing HTTP tests** (`tests/http.rs`, using `axum::body` + `tower::ServiceExt::oneshot`, no real socket): build a router over fake/real services with one test user and a 2-file `testdata` library. Assert: `ping` → 200 + version; `auth` good → 200 + token, bad → 401; `index` without token → 401, with token → 200 + non-empty body + an `ETag`; repeat `index` with `If-None-Match: <etag>` → 304; `track/0` with token + `Range: bytes=0-3` → 206 + 4 bytes matching the file head; `track/999` → 404.
- [ ] **Step 2 — run FAIL; implement the router + extractors; run PASS.**
- [ ] **Step 3 — wire `serve`** in the bin to bind `cfg.bind` and run the router (manual, not unit-tested here — covered by E2E).
- [ ] **Step 4 — commit.** `feat(server): native /v0 HTTP protocol (auth, index+ETag, track Range, art)`.

---

## Increment D — client: remote source in soul-player-host

### Task D1: `Library::locate` in the backend (source index + root-relative path)

**Files:** `crates/soul-player-backend/src/library.rs`.

**Interfaces — Produces:** `pub struct Located { pub source: usize, pub rel: PathBuf }` and `pub fn locate(&self, id: u32) -> Option<Located>` — the same computation as `path_of` but returning the source index + the root-relative path (NOT joined to `roots[source]`).

- [ ] **Step 1 — failing test** (mirror the existing `path_of` test): after `scan_many(&[root])`, `locate(0)` → `Some(Located{ source: 0, rel })` where `root.join(rel) == path_of(0).unwrap()`; `locate(9)` (OOB) → `None`.
- [ ] **Step 2 — run FAIL; implement `locate` (refactor `path_of` to share the folder/rel computation); run PASS** (`cargo test -p soul-player-backend`).
- [ ] **Step 3 — commit.** `feat(backend): Library::locate — source index + root-relative path`.

### Task D2: `remote` module — `TrackLocation`, `MediaFetch`, `RemoteSource` sync/fetch

**Files:** `crates/soul-player-host/src/remote.rs` (new); Modify `crates/soul-player-host/Cargo.toml` (`reqwest = { version = "0.12", features = ["blocking","json"] }`; `soul-server` is NOT a dep here — the client speaks raw HTTP), `src/lib.rs` (`mod remote;`).

**Interfaces — Produces:**
```rust
pub struct RemoteServer { pub url: String, pub token: String }  // token obtained at add-time
pub enum TrackLocation { Local(PathBuf), Remote { server: usize, rel: PathBuf, track_id: u32 } }
pub trait MediaFetch: Send + Sync { fn fetch(&self, server: &RemoteServer, track_id: u32) -> anyhow::Result<PathBuf>; } // returns a local cache path
pub struct DownloadCache { dir: PathBuf }   // impl MediaFetch: GET {url}/v0/track/{id} -> cache file (skip if cached)
// client calls:
pub fn authenticate(url: &str, user: &str, pass: &str) -> anyhow::Result<String>; // POST /v0/auth -> token
pub fn fetch_index(server: &RemoteServer) -> anyhow::Result<Vec<u8>>;             // GET /v0/index -> .soulidx bytes
```
- [ ] **Step 1 — failing test against a local server:** in `remote.rs` tests, start the real `soul-server` router on an ephemeral `tokio` socket (dev-dep `soul-server = { path = "../soul-server" }`, `tokio`) seeded with a temp user + `testdata` library; assert `authenticate` returns a non-empty token for the right creds and errors for wrong; `fetch_index(server)` returns bytes that `Library::from_source_bytes(&[marker_root], vec![bytes])` accepts (len > 0); `DownloadCache.fetch(server, 0)` writes a cache file whose bytes equal the server file, and a second fetch is a cache hit (no re-download — assert mtime unchanged or an injected counter).
- [ ] **Step 2 — run FAIL; implement `authenticate`/`fetch_index`/`DownloadCache` with `reqwest::blocking`; run PASS** (`cargo test --manifest-path crates/soul-player-host/Cargo.toml remote`).
- [ ] **Step 3 — commit.** `feat(host): remote module — auth, index fetch, download-to-cache`.

### Task D3: aggregate remote sources + branch the loader

**Files:** `crates/soul-player-host/src/lib.rs`, `crates/soul-player-backend/src/actor.rs` (loader), `crates/soul-player-backend/src/library.rs` (if the loader needs `locate`).

**Interfaces — Produces:** the host keeps `remotes: Vec<RemoteServer>` parallel to the remote roots; builds the aggregate `Library::from_source_bytes(all_roots, all_bytes)` with remote roots using a sentinel marker and remote bytes from `fetch_index`; a `source_kind: Vec<SourceKind>` map (`Local | Remote(idx)`). The loader resolves `locate(id)` → if the source is Remote → `MediaFetch.fetch(server, track_id)` → `open_track(cache)`; else `open_track(root.join(rel))`.

- [ ] **Step 1 — failing test:** host test that adds a local root AND a remote server (local in-process server), builds the aggregate, asserts `source_counts()`/`views()` include BOTH sources' tracks, and that resolving a known remote track yields a playable cache path (decode ≥1 block). (This overlaps the E2E but at the host-unit level.)
- [ ] **Step 2 — run FAIL; implement the aggregation + loader branch; run PASS.**
- [ ] **Step 3 — commit.** `feat(host): aggregate remote sources + remote-aware loader`.

---

## Increment E — rune Remote add-source UI

### Task E1: Enable the Remote add-source item + connect form

**Files:** `app/rune/src/views/settings.rs`, `app/rune/src/main.rs`, `app/rune/locales/app.yml`.

**Interfaces — Consumes:** the host API from D (add a remote source given url/user/pass). **Produces:** the "Remote server" add-source item becomes focusable → opens a small form (server URL, username, password, Connect); on Connect it calls the host to authenticate + add the source; on success the form closes and the source appears in Settings → Sources (reuse the existing rich source rows).

- [ ] **Step 1 — failing test** (rune headless): a test that opens the add-source menu, clicks "Remote server", asserts a form with keyed fields `remote-url`/`remote-user`/`remote-pass`/`remote-connect` renders (and is focusable/labelled — the a11y sweep will also cover it). (Behavioural connect is covered by E2E; here assert the UI exists + is reachable.)
- [ ] **Step 2 — run FAIL; implement the form + enable the item (remove the "soon"/disabled state); localize the new strings across every `app.yml` locale; run PASS** (`cargo test --bin soul-player-rune` in `app/rune`).
- [ ] **Step 3 — commit.** `feat(ui): enable the Remote add-source form`.

---

## Increment F — the E2E acceptance test

### Task F1: add → sync → browse → play E2E

**Files:** `crates/soul-player-host/tests/remote_e2e.rs`; Modify `crates/soul-player-host/Cargo.toml` (`[dev-dependencies] soul-server = { path = "../soul-server" }`, `tokio`, `tempfile`).

- [ ] **Step 1 — the test:**
```
1. temp dir; copy a few testdata/ files into a "server library" root; open a temp SqliteUsers, add user ("e2e","pw"); build Services{CoreIndex::scan(root), FsMedia, users}; spawn soul-server::proto::native::serve on 127.0.0.1:0 (ephemeral) on a tokio runtime; read back the bound port.
2. host side: authenticate(url,"e2e","pw") -> token; add the remote source; sync fetch_index; build the aggregate Library.
3. ASSERT browse: the aggregate views()/source_counts() list the expected number of tracks and a known title from the server library.
4. ASSERT play: pick a known remote track id; resolve TrackLocation::Remote; MediaFetch.fetch -> cache; open_track(cache) decodes >= 1 block of audio (or the decoded duration matches the known file). 
```
- [ ] **Step 2 — run, iterate until green.** `cargo test --manifest-path crates/soul-player-host/Cargo.toml --test remote_e2e`.
- [ ] **Step 3 — commit.** `test(remote): add->sync->browse->play E2E`.

### Task F2: full verification

- [ ] `cargo fmt --all` + `cargo clippy --all-targets -- -D warnings` clean in the root workspace AND `crates/soul-player-host` AND `app/rune`.
- [ ] Full suites green: root (`cargo test`), host (`cargo test --manifest-path crates/soul-player-host/Cargo.toml`), rune (`cargo test --bin soul-player-rune` in `app/rune`).
- [ ] Merge latest `main`; re-verify. Ready for PR.
