# Remote music source — E2E vertical slice (native protocol) — design

Program: Sources overhaul **Phase 3 (remote)**. The add-source menu already has a disabled "Remote" slot.
Date: 2026-11-16
Branch: `feat/remote-source-server`

## Goal

A self-hostable **`soul-server`** that serves a music library over a native, bit-perfect protocol, and **`soul-player-ng`** mounting it as a **Remote** source. Proven end-to-end by one test: **add a remote source → sync its `.soulidx` → browse its albums → play a track.**

This is the first vertical slice through a **hexagonal** server built so more protocols (OpenSubsonic, …) bolt on later as adapters with zero domain change.

## Architecture — hexagonal `soul-server`

The domain is protocol-agnostic and speaks only rich audiophile types. Ports are Rust **traits**; adapters are **impls**; the binary wires them in `main`.

```
 INBOUND (driving) adapters         DOMAIN (ports + use cases)      OUTBOUND (driven) adapters
  native proto (HTTP) ──▶ Catalog  (list/get)   ──▶ IndexPort   → soul-audio-core scan/.soulidx
  CLI / admin         ──▶ Auth     (verify/issue)──▶ MediaStore  → filesystem (open Read+Seek)
  (later: subsonic)   ──▶ Streaming(open range) ──▶ UserStore   → SQLite (users, Argon2id, tokens)
                           Admin    (user CRUD)
```

**One crate, `soul-server`, root-workspace member**, with modules enforcing the hexagon:
`domain/` (types + port traits + use cases, no I/O) · `index/` (IndexPort over `soul-audio-core`) · `store/` (UserStore over SQLite) · `media/` (MediaStore over fs) · `proto/native/` (HTTP adapter) · `bin/soul-server.rs` (CLI + config + wiring). Promote `proto/native` to its own crate when a second protocol lands.

### Ports (traits)
- `IndexPort`: `scan(roots) -> IndexVersion`; `index_bytes() -> (Vec<u8> /*.soulidx*/, Etag)`; `locate(track_id) -> RelPath`. Backed by core's scan (the same `.soulidx` the desktop builds).
- `MediaStore`: `open(rel_path) -> impl Read + Seek` + `len`/`content_type`. Filesystem adapter.
- `UserStore`: `verify(user, pass) -> Option<UserId>` (Argon2id); `issue_token(user) -> Token`; `check(token) -> Option<UserId>`; `add_user`/`remove_user`. SQLite adapter.
- Use-case services (`Catalog`, `Auth`, `Streaming`, `Admin`) compose these behind the inbound API the protocol adapter calls.

## Native protocol v0 (the optimized first-class adapter)

HTTP/1.1, JSON for control, raw bytes for media. All non-auth routes require `Authorization: Bearer <token>`.

| Method & path | Does | Notes |
|---|---|---|
| `GET /v0/ping` | health + server version + library version/etag | unauthenticated |
| `POST /v0/auth` | `{user,pass}` → `{token, expires}` | Argon2id verify; opaque bearer token |
| `GET /v0/index` | the library `.soulidx` bytes | `ETag: <version>`; `304` on `If-None-Match` match → client skips re-sync |
| `GET /v0/track/{id}` | original file bytes | **supports `Range`** (206); `Content-Type` from codec; no transcode |
| `GET /v0/art/{album_id}` | cover-art bytes | `404` when absent |

Bit-perfect by construction: `/v0/track` serves the untouched file; the client decodes it with the existing pipeline.

## Client (`-ng`) Remote source

- **Config / auth**: a remote source is `{ url, user }` persisted in config; the password is exchanged once for a bearer token at add-time. Token stored via the OS keychain where available, else the app config dir (documented). (Keychain wiring may be a follow-up; v0 may store the token in the app config dir — noted as a security follow-up, not a blocker.)
- **Index sync**: on connect / refresh, `GET /v0/index` (with `If-None-Match`) → write the fetched `.soulidx` to a per-source cache file → feed it into the host's existing **MultiCatalog** exactly like a local source's index. Albums/artists/tracks then appear through the normal library path.
- **Track location**: the backend loader today resolves `id → PathBuf` then `open_track(path)`. Generalise to `id → TrackLocation` = `Local(PathBuf)` | `Remote { source, track_id }`. A `MediaFetch` port resolves a `Remote` location to a decodable source:
  - **v0 (this slice): download-to-cache.** `GET /v0/track/{id}` → write to a temp cache file → `open_track(cache_path)` (the existing, path-based pipeline — unchanged, bit-perfect). Prefetch the next queued track for gapless.
  - **follow-up (not this slice): range-stream.** An HTTP-Range-backed `Read + Seek` → core's `open_seekable` (needs core to surface `open_seekable` publicly). The `MediaFetch` port makes this a drop-in swap.
- **UI**: the add-source "Remote" item becomes enabled → a small form (server URL, username, password, Connect). On success the source shows in Settings → Sources (reusing the rich source rows) with its track count; its library merges into the normal albums/tracks views; play works.

## Core dependency

**None required for this slice** — download-to-cache reuses the existing `open_track(path)`. (The streaming follow-up will need `open_seekable` exposed from `soul-audio-desktop`; tracked separately so the 3-workspace core bump isn't on this slice's critical path.)

## E2E test (the acceptance gate)

`crates/soul-player-host/tests/remote_e2e.rs` (host dev-deps `soul-server` by path):
1. Build a tiny library from `testdata/` in a temp dir; start `soul-server` in-process on an ephemeral port with one test user.
2. From the host: add the remote source with the URL + credentials; authenticate; sync the index.
3. Assert the catalog now lists the expected albums/tracks from the test library (counts + a known title).
4. Play a track: resolve its `Remote` location → fetch → `open_track` → decode ≥1 block of non-silent audio (or assert the decoded duration matches), proving the full add→sync→browse→play path.

## Decisions (resolved from the design discussion)

- Native catalog = **ship `.soulidx` + ETag** (index-sync, not a query API). Querying is a future adapter's job.
- Domain = **thin view over core types**; the native adapter raw-passes the `.soulidx`. Core stays the scanning authority.
- **SQLite** user store (single file; Argon2id password hash; a reserved column for a future separate Subsonic secret so the facade never touches the real hash).
- **TLS via reverse proxy** (documented); a dev `--insecure` plain-HTTP mode. No built-in TLS in v0.
- Server packaging: a single static binary + a `soul-server.toml` (library roots, bind addr, db path); `soul-server serve` / `soul-server user add <name>`.

## Scope (YAGNI)

**In:** the native protocol v0, one-or-many library roots, password auth + bearer tokens, full-index sync, Range-capable byte serving, cover art, the enabled Remote add-source UI, and the E2E test.

**Out (later sub-projects / follow-ups):** the OpenSubsonic facade; the client mounting foreign Subsonic servers; incremental index **deltas** (v0 ships the whole `.soulidx` each sync); range-stream playback via `open_seekable`; OS-keychain token storage; multi-user roles/permissions beyond "can access"; a web admin UI; built-in TLS; server-side transcoding.

## Testing strategy

- **Server**: domain unit tests (use cases over fake ports); HTTP-level adapter tests (auth success/failure, `/index` + ETag/304, `/track` Range → 206, 401 without a token).
- **Client**: `TrackLocation` resolution; the download-to-cache `MediaFetch`; index-sync writes a usable catalog.
- **E2E**: the add→sync→browse→play test above (the acceptance gate).

## Risks

- **3-workspace churn**: `soul-server` joins the root (service) workspace → covered by the `rust (service + host)` CI leg; the host gains a path dev-dep on it. No app/rune change except enabling the Remote UI (its own workspace).
- **Decode-from-download latency** for very large hi-res/DSD files (full download before play) — acceptable for v0 and for the local E2E; the range-stream follow-up removes it.
- **Token-at-rest**: storing the bearer token in the config dir (v0) is weaker than the keychain — documented as a follow-up, and tokens are revocable server-side.
