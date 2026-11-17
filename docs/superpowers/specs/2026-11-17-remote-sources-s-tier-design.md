# S-tier remote sources — spec (sub-project 2 + streaming)

Builds on PR #214 (OAuth2.1 auth). Branch: `feat/remote-sources-s-tier` off main (after #214 merges).
Goal: make remote sources a first-class, S-tier experience — visible health/telemetry, clear
local/remote distinction, true streaming, persistence + reconnect.

## Design decisions (controller)

- **Health = client polls `GET /v0/ping`** (unauth; returns `{server: version, index_version}`).
  Latency = round-trip ms. Online = a recent ping succeeded. `index_version` mismatch vs last
  sync ⇒ "update available" (cheap re-sync trigger). Poll off-thread on an interval (~30 s) +
  on demand; report to UI via the `AppHandle::update` pattern (mirror `scan_in_background`).
- **Source model**: generalize beyond folder-centric. Introduce `SourceKind = Local{path} |
  Remote{url}` and a `RemoteHealth { online, latency_ms, server_version, index_version,
  last_checked_unix, last_sync_unix }`. Keep `SourceStatus { enabled, available, track_count }`
  and add `kind` + `remote: Option<RemoteHealth>`. Local sources: `remote = None`.
- **Library local/remote marker**: `TrackView` gains `source: Option<SourceRef>` (or a `remote:
  bool` + source name). Derive during projection from the compound id (`pack(source,local)` →
  which aggregate source → is it a remote block?). UI shows a small remote badge.
- **Streaming**: add an HTTP-Range-backed `Read+Seek` reader over `GET /v0/track/{id}` → core's
  `open_seekable`. Requires exposing `open_seekable` publicly from core (`soul-audio-desktop`).
  Stream by default; keep download-to-cache as a fallback (tiny files / non-seekable). Prefetch
  next queued track for gapless.
- **Persist**: remote sources in `config.toml` (`url`; the refresh token already lives in the
  keychain). On startup: reload → silent refresh (refresh-token grant) → re-sync index → start
  the health poller. Offline remotes list with 0 tracks + an offline indicator, reconnect when
  the poller sees them return (the local "don't wipe on unplug" rule, applied to remotes).

## Increments (the loop)

- **Inc 1 — health model + poller (host/backend).** `SourceKind`, `RemoteHealth`, extend
  `SourceStatus`; a `RemoteHealth` probe (`ping` → latency/version); an off-thread poller. TDD
  against the in-process server (latency measured, version read, offline→online transition).
- **Inc 2 — Sources data/health panel (rune).** Rows show Local/Remote badge, online/offline
  dot, latency ms, server version, track count, last sync. Reuse the rich source rows; a details
  area for the data. Localized. Test via `rt.elements()`.
- **Inc 3 — library local/remote indicator.** `TrackView.source`; projection derives it; a badge
  in track rows + now-playing. Backend + rune tests.
- **Inc 4 — true `open_seekable` streaming.** Core PR: expose `open_seekable`. -ng: Range reader
  + `MediaFetch` streaming impl; stream-by-default, cache fallback; gapless prefetch. 3-workspace
  lockfile bump. signal_e2e / remote_e2e prove streamed playback.
- **Inc 5 — persist + auto-reconnect.** Config persistence; startup reload + silent refresh +
  re-sync; offline/reconnect UX driven by the poller.
- **Inc 6 — S-tier polish.** Latency history/sparkline, graceful degradation, error surfacing,
  remote gapless. Iterate until S-tier.

## Testing bar per increment
TDD; `cargo test` green on each touched workspace; `remote_e2e`/`signal_e2e` extended for
streaming; cargo audit/deny clean on any new dep; CI green cross-platform before moving on.
