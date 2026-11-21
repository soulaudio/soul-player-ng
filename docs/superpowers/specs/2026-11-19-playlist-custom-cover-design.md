# Custom Playlist Cover Upload

**Status:** approved (playlist fast-follow, user picked this subset)
**Date:** 2026-11-19
**Builds on:** playlists (#229), heart/membership (#231), detail-row actions (#233)

## Goal

Let a user set a custom cover image on a playlist (overriding the auto 2x2
mosaic), and remove it to revert. The `custom_cover` field and the per-playlist
folder layout were reserved for this in #229. App-only; no backend/host/core.

## Behavior

- **Set cover:** a "Set cover" action on the playlist detail hero opens the
  native image file dialog (`tx.open_file` with an image filter). On pick, the
  chosen image is copied into the playlist's own folder as `cover.<ext>` (ext
  preserved, lowercased), `playlist.toml`'s `custom_cover` is set to the file
  name, and it persists. Validation: accept jpg/jpeg/png/webp/gif only; reject
  files over a size cap (e.g. 16 MiB) with no change (best-effort, never panics).
- **Remove cover:** a "Remove cover" action (shown only when a custom cover
  exists) deletes the `cover.*` file, clears `custom_cover`, persists, and the
  cover reverts to the auto mosaic/placeholder.
- **Precedence (everywhere a playlist cover shows):** `custom_cover` (if set and
  the file exists) > auto 2x2 mosaic > single cover > gradient/icon placeholder.
  Honored on BOTH the Playlists list page thumbnails and the playlist detail
  hero.

## Design

### Store (`app/rune/src/playlists/`)
- `PlaylistStore::set_custom_cover(id, src: &Path) -> bool`: validate extension +
  size; copy `src` into `<playlist-folder>/cover.<ext>` (removing any prior
  `cover.*`); set `playlist.custom_cover = Some("cover.<ext>")`; persist; return
  whether it was applied.
- `PlaylistStore::clear_custom_cover(id)`: delete the `cover.*` file, clear the
  field, persist.
- `custom_cover_path(id) -> Option<PathBuf>`: the absolute path to the current
  cover file, if `custom_cover` is set and the file exists (used by rendering).
- `PlaylistSummary` gains `custom_cover: Option<PathBuf>` (absolute path, resolved
  by `list()`), so the list page can render it without loading the full playlist.

### Rendering (`app/rune/src/views/`)
- A small **path-keyed image cache** for cover files (mirror `player::ArtCache`'s
  `ImageCache` usage: read file bytes, `load_mime(bytes, mime_from_ext, px,
  scale)`, memoize by path). Wire its `Waker` in `AppSpec::setup` like `ArtCache`,
  OR reuse `ArtCache`'s `ImageCache` with a path-keyed entry. Owned by `State`.
- `playlists::cover`: add `PlaylistCover::Custom(PathBuf)`. The chooser checks
  `custom_cover` first; if present+loadable -> `Custom(path)`, else the existing
  mosaic/single/placeholder logic.
- `views/playlists.rs` `playlist_cover_el`: render `Custom(path)` via the cache
  (an `Image` filling the thumbnail square), falling back to placeholder if the
  file fails to load.
- `detail_page.rs`: add `HeroArt::Image(PathBuf)` + render it in `hero_artwork`
  via the same cache. `playlist_detail.rs` selects `HeroArt::Image` when a custom
  cover exists, else the existing cover-track / icon logic.

### UI (`app/rune/src/views/playlist_detail.rs`)
- On the detail hero (or its action area): a "Set cover" button -> `tx.open_file`
  with an image filter -> `set_custom_cover`. When a custom cover exists, also a
  "Remove cover" button -> `clear_custom_cover`. Keyed (`playlist-set-cover` /
  `playlist-remove-cover`) for tests.
- Localize `playlists.setCover` / `playlists.removeCover` + a dialog title key in
  6 locales.

## Testing (TDD)

- Store: `set_custom_cover` copies the file into the playlist folder, sets the
  field, persists (fresh `load()` sees it); rejects a non-image extension and an
  over-cap file (no change); `clear_custom_cover` deletes the file + clears the
  field; `custom_cover_path` returns the path only when the file exists;
  `PlaylistSummary.custom_cover` is populated by `list()`.
- Rendering: `playlist_cover` returns `Custom` when a cover file is set (precedence
  over mosaic); falls back when absent/unloadable.
- UI: the "Set cover" action opens the file dialog and, via the scripted-dialog
  test harness (the `tx.open_file` pattern used by the EQ/profile-import tests),
  a picked path calls `set_custom_cover`; "Remove cover" calls `clear_custom_cover`
  and shows only when a cover exists.
- app render e2e: set a cover on a playlist -> the detail hero + the list row
  render the custom image (element present) -> remove -> reverts.

## Out of scope

Drag-and-drop image onto a playlist; in-app crop/zoom; cover sync to remote/DAP;
AI-generated covers; per-track custom art. (M3U8 export + drag-reorder remain
separate deferred fast-follows.)

## Global constraints

- App-only: modify only `app/rune` (`playlists/{store,cover,mod}.rs`,
  `views/{playlists,playlist_detail,detail_page}.rs`, `main.rs` for the cache +
  State + AppSpec::setup wiring, `locales/app.yml`, `i18n_tests.rs`, tests). No
  backend/host/core.
- No em-dashes; no attribution in commits; strings localized in 6 locales.
- The file dialog is Rune's `tx.open_file`/`save_file` (as Settings uses). TDD;
  app/rune nightly; clippy `--all-targets`.
