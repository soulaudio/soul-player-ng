# Playlists Design

**Status:** approved (brainstorm complete)
**Date:** 2026-11-19
**Issue:** #101 (Playlists — create / edit / persist)

## Goal

Add user-authored playlists to the Rune desktop head: create, rename, delete,
add tracks, remove tracks, reorder, and persist them; browse them from a new
Playlists nav page with auto-generated cover mosaics; play a playlist or add its
tracks to the queue. Designed so the planned DAP head can consume the same
playlists read-only via the existing baked `.soulidx` playlist table (Phase 2).

## Why this is harder than it looks (the constraint that shapes everything)

Internal track ids in this app are **volatile**. A track id is a compound
`u32` = `source << 28 | local`, where `local` is just the track's position in a
baked index. That position **renumbers on every rescan** (add/remove/reorder a
file and everything after it shifts), and the `source` bits shift when sources
are enabled/disabled/reordered or a remote re-bakes. The `.soulidx` index is a
derived, rebuilt-every-scan artifact that is not even persisted to disk on the
desktop (it is baked in memory per watched folder).

Therefore **a playlist must never store an internal track id** — it would
silently point at the wrong song after the next scan. This is the storage-layer
version of the lesson already learned in the queue (dup track-ids crashed the
Rune UI; queue rows key by position). Playlists reference tracks by a **durable
locator + a metadata snapshot**, resolved to a current id only at use time.

This design is corroborated by how mature players handle the same problem
(Navidrome's path-match-then-persistent-id, XSPF's `location`/`identifier`
split, Roon's relative-path matching, Plex's guid-over-ratingKey, and
Music Assistant moving playlists into its own DB the moment local + remote
sources had to coexist in one list).

## Architecture: a read/write split

- **Author (write side) = the Rune app.** The editable playlist store lives in
  `app/rune`, following the existing `config.rs` pattern (serde files in a
  per-user directory, load-modify-write). Editable playlists are user-authored
  mutable state, a desktop concern.
- **Resolve = the live catalog.** The backend/host gain two small methods to
  map a durable locator to the current track id (and back), so playback and the
  UI can reach real tracks.
- **Consume (read side, Phase 2) = the DAP.** The DAP reads the baked
  `.soulidx` playlist table, which already exists in core (format v5) with
  read-only trait methods. Phase 2 populates that table at bake time by
  resolving the desktop locators against the device index. **Phase 1 does not
  touch core at all** (no rev bump, no core worktree).

This mirrors the established "the DAP reads what -ng bakes" layering used for
audio profiles and the managed-library/organize split.

## The durable reference: locator + snapshot

Each playlist entry stores:

```
entry:
  locator:  Local  { path }                    # absolute filesystem path
          | Remote { server, rel }             # server base URL + server-relative path
  snapshot: { title, artist, album, duration_secs, track_no?, disc_no? }
  mbid:     Option<String>                      # captured opportunistically when present in tags
```

- The **locator** is the durable key (local absolute path = the same key
  `loudness_cache.txt` already uses; remote `(server, rel)` mirrors the existing
  local-vs-remote source model). It is never a volatile id.
- The **snapshot** lets a row render (title/artist/album) even when the track is
  currently unavailable (source offline, file moved) — so rows are **greyed out,
  not dropped**. Dropping would reorder the remaining tracks and lose position.
- The **mbid** is an opportunistic second signal for relink (free when tags
  carry it); never the sole key (coverage is inverse to need).

### Resolution (locator -> current id), on load and after each rescan

Each step is a cheap exact check before the next:

1. **Exact locator hit** in the fresh catalog (same absolute path, or same
   `(server, rel)`) -> resolve directly. This is the 99% case (nothing moved).
2. **MBID hit** elsewhere in the catalog, if the snapshot captured one ->
   auto-relink silently (MBID is content-identity; safe).
3. **Single unambiguous metadata-snapshot match** (title+artist+album, duration
   within a small tolerance) -> relink. If more than one candidate matches,
   **do not guess** (every mature player treats silent fuzzy reassignment as
   worse than a visibly-broken row).
4. **No match** -> keep the entry, render it from the snapshot, mark it
   unresolved (greyed, non-interactive for play/drag).

**Whole-library-root move fast path:** when a local source's root folder moves
but its internal structure does not (detected via the existing scoped-watcher
"root changed" signal), rewrite stored absolute paths by swapping the old root
prefix for the new one in one O(1) pass, instead of forcing every entry through
steps 2-4.

### How this interacts with indexing (it does not add scan work)

- `playlists` files are **never scanned or indexed** — they are deserialized
  once at startup like `config.toml`. The file scanner / `.soulidx` baker is
  unchanged.
- Resolution needs the reverse map `path -> id`. We build it **once per catalog
  load** by walking the catalog — the same walk the UI already does to build the
  `TrackView` list — and cache it; resolving a playlist is then N hashmap
  lookups (N = entries in the playlist, typically tens). No per-frame cost.
- Timing fits the existing progressive startup: load the playlist files
  instantly and render rows from their snapshots; resolve to live ids when the
  off-thread scan completes, and re-resolve on each rescan.
- **Path normalization** is the one real gotcha (Windows case-insensitivity,
  separators, long paths). Reuse the normalization already used by
  `loudness_cache.txt`'s path keying rather than inventing a new one.

## Storage layout

One **self-contained folder per playlist** (bundles the definition and, later,
its custom cover), under a `playlists/` directory:

```
<store>/playlists/
  index.toml                      # manifest: [{ id, slug, name, order, count, cover_ids[<=4] }]
  <slug>-<id>/
    playlist.toml                 # { id, name, custom_cover?, entries: [...] }
    cover.<ext>                   # OPTIONAL custom cover, only when the user sets one (v2)
  ...
```

- **One file per playlist** avoids write amplification: editing playlist A
  rewrites only A's `playlist.toml`, never B's.
- **Folder name is `<slug>-<id>`**, not a bare slug:
  - The `id` is a **stable** identity (a counter persisted in the manifest as
    `next_id`). It lives in the folder-name suffix and inside `playlist.toml`.
    Playlist names are not unique and can change, so identity must not be the
    name. (`State.open_playlist` holds this id, not a name.)
  - The `slug` is sanitized from the name (lowercased, path-illegal chars and
    whitespace -> `-`, reserved names avoided, truncated ~40 chars) purely for
    on-disk legibility. The app never keys on the slug. If a name sanitizes to
    empty, the folder is just `<id>`. On rename, update the toml `name` (and
    re-slug the folder for tidiness); a stale folder slug is harmless because
    identity is the id.
- **`index.toml` manifest** powers the cheap Playlists list + cover mosaics:
  it carries name, count, and the <=4 cover track locators/ids per playlist, so
  the list page never loads every playlist's full entry list (the "too big"
  guard). It is a rebuildable cache — if stale (crash mid-write), regenerate it
  by scanning the folders.
- A playlist folder **never contains audio** — only its `playlist.toml` and an
  optional `cover.*`. Audio lives in the library; entries reference it by
  locator.

### Store location (one resolver, managed-library aware)

`playlist_store_path()` resolves the base:

- **Managed library present** -> inside it: `<managed-library>/.soul/playlists/`.
  Playlists then travel with the library (move/back-up/share the folder) and are
  a natural unit of a DAP sync.
- **Otherwise** -> `soul_player_dir()/playlists/`.

The managed-library/organize crate is a future phase, so **Phase 1 writes to the
config-dir location**, but behind this resolver. When the managed library ships,
the resolver returns the in-library base and a one-time migration moves existing
playlists in; no model change is needed (durable locators make a playlist valid
regardless of which base it sits under).

## Scale

- Per-folder isolation -> no cross-playlist write amplification.
- Manifest-driven list -> list rendering is O(playlists), independent of total
  tracks. A playlist's full entries load only when it is opened.
- A pathologically large single playlist (tens of thousands of entries) only
  slows its own open; realistic sizes (tens-thousands) are fine as TOML. If that
  ever becomes real, swap just that file's encoding to compact binary (postcard,
  like `scan_caches.bin`) — an isolated change, not a v1 concern.

## Cover art

- **Auto 2x2 mosaic**, rendered **live** (never baked to a file — sidesteps the
  stale-cover bug): walk entries in order, collect the first **4 distinct album
  covers** (dedupe by album identity, falling back to deduping by the `art(id)`
  result; skip entries with no art) -> 2x2 grid, centered-square crop per tile,
  no gutter, in order.
- **1-3 distinct covers** -> render the **single first cover** full-bleed, not a
  padded/partial grid (every serious reimplementation calls the partial grid
  "broken").
- **0 covers** -> a generated gradient placeholder (hue derived deterministically
  from the playlist id for stability) plus a generic playlist glyph.
- **Precedence:** `custom_cover` (if set) > live mosaic > placeholder. A
  `custom_cover: Option<...>` field is **reserved in the model now** (checked
  before the mosaic) but upload UI is deferred to v2; the folder layout already
  accommodates the `cover.*` file.
- **No file-bake pipeline.** Memoize only the <=4 chosen cover ids, keyed on a
  hash of the playlist's current entry list (recomputed only when membership
  changes); the existing per-track art path supplies the pixels.

## UI surface (all in `app/rune`)

- **Sidebar:** a new `Playlists` nav entry, with the usual per-item `nav_hidden`
  toggle and `nav_label`/`nav_active` arms. New `Route::Playlists` and
  `Route::Playlist`; `State.open_playlist` holds the stable playlist id;
  `NavEntry` / `navigate_to` / `go_back` extended for the new routes.
- **Playlists page:** a scrolled list (or grid) of playlists, each showing its
  mosaic thumbnail, name, and track count, plus a **New playlist** affordance
  (inline text entry — proven viable via the remote connect-form). Row ->
  playlist detail.
- **Playlist detail:** reuse the shared `detail_hero` + `play_all_pill`
  primitives; a track-row list (reuse `detail_track_row`) with per-row remove;
  **rename**, **delete**, and **reorder via move-up / move-down** controls.
  Unavailable entries render greyed and non-interactive for play/drag.
  `play_all_pill` resolves the playlist's locators to current ids (skipping
  unavailable) and issues the existing `Command::PlayContext { ids, start }`.
- **Add to playlist:** on existing track-row context menus (library, genre
  detail, album detail), an in-app themed overlay listing the user's playlists +
  an inline "New playlist..." field. Uses the app's overlay/dropdown pattern (not
  the native popup) because it needs text input.
- **Playback:** reuses `Command::PlayContext` (Play All), `PlayNext`, `Enqueue`.
  **No new playback commands.**
- **i18n:** all new strings in the 6 locales (en/de/es/zh/cs/ja).

## Backend / host additions (the only cross-workspace work in Phase 1)

- **backend (`soul-player-backend`):**
  - `Library::locator_of(id) -> Option<TrackLocator>` (uses the existing
    `locate`/`path_of`; `Remote` for remote tracks).
  - `Library::resolve_locators(&[TrackLocator]) -> Vec<Option<u32>>` (builds the
    `path -> id` reverse map from the current catalog once, then looks each up;
    includes the MBID / single-metadata-match fallbacks).
- **host (`soul-player-host`):** `PlayerHandle` pass-throughs for both, so
  `app/rune` can reach them.

`TrackLocator` and the snapshot DTO are defined once (backend `types.rs`) and
re-exported through the host, consumed by the app store.

## Phase 2 (documented, not built in this pass): DAP export

The seam exists so Phase 1 is provably compatible:

- A **bake-time resolver** converts the desktop playlists (durable locators) into
  the baker's `PlaylistInput { name, tracks: Vec<u32> }` by resolving each
  locator against the specific index being baked (ids are stable *within* one
  immutable baked index, so the volatility problem does not exist on the DAP).
- The baker and the DAP read trait methods (`playlist_count` / `playlist_name` /
  `playlist_len` / `playlist_track`) already exist in core — only the bake-path
  population and the device-sync seam are new (a small isolated core change +
  rev bump, done the established way).
- Multi-source nuance (a desktop playlist spanning several sources collapses to
  the device's single on-device library; unsynced tracks are omitted) lives in
  Phase 2.
- Two distinct exports, do not conflate: DAP <- `.soulidx` playlist table
  (binary, native); other players <- M3U8 (text, write-only, deferred).

## Testing (TDD)

- **Store round-trips:** create / add / remove / reorder / rename / delete ->
  save -> reload yields the same state; per-playlist file isolation (editing A
  does not rewrite B); manifest rebuild from folders.
- **Slug/id:** illegal-char names sanitize safely; duplicate names get distinct
  folders; rename preserves id; `State.open_playlist` keys on id.
- **Locator resolution survives a rescan that renumbers ids** (the key
  regression guard): bake a library, resolve a playlist, add a file to force
  renumber, re-resolve, assert the playlist still points at the same tracks.
- **Unavailable degradation:** a locator with no match renders from snapshot,
  greyed, non-interactive; Play All skips it.
- **Cover mosaic selection:** first-4-distinct (dedupe), 1-3 -> single cover,
  0 -> placeholder; memoization invalidates on membership change.
- **App render e2e:** new playlist -> add track -> appears on the Playlists page
  with the right count/cover -> open -> Play All issues `PlayContext` with the
  resolved ids in order.

## Out of scope for v1 (YAGNI)

Smart / rule-based playlists; drag-reorder (move-up/down only); custom-cover
**upload** UI (field + folder layout reserved); M3U8 import **and** export;
cross-device sync beyond the Phase 2 DAP bake; moving the editable model into a
shared core crate (`soul-audio-mutable`, still unbuilt); audio fingerprint
relink.

## Global constraints

- **Phase 1 touches: `soul-player-backend`, `soul-player-host`, `app/rune`.
  `soul-audio-core` is NOT modified** (no rev bump). Three `-ng` workspaces;
  run cargo from the correct workspace dir; `app/rune` builds on
  nightly-2026-11-10 (auto-selected from its CWD).
- Commit messages carry **no attribution** (no Claude/Anthropic/AI/Co-Authored-By
  — the hook rejects them).
- **No em-dashes** in UI copy, prose, or commits (use commas / colons / parens /
  middots); do not mass-rewrite deep code comments.
- All user-facing strings localized in 6 locales.
- The themed in-app dropdown/overlay is the only way to build a dropdown (an arch
  test bans the native `PopupMenu::picker`); the existing `views::dropdown` /
  `El::anchored` / `El::modal` patterns apply.
- TDD throughout; frequent commits; DRY; YAGNI.
