# Content / library pages — Album, Artist, Genre (match the original) — design

Date: 2026-11-16
Status: approved direction, pre-implementation

## Goal

Upgrade the Rune head's **Album, Artist, and Genre** pages to look and behave like the
original soul-player (React/Tauri), and do it by **reusing shared components** the same
way the original did (`MediaCard` → card variants; `DetailPageLayout` hero; one
`TrackList`; a responsive grid). The pages already exist in a basic form; this makes them
feel like the same app and removes the per-page duplication.

Playlists are a **separate follow-up project** (net-new persistence + host commands), out
of scope here — which also means the original's "Add to playlist" actions are omitted
from this project.

## Current state (what exists)

`app/rune/src/views/{albums,artists,genres}.rs` each already contain a browse view **and**
a detail page, dispatched via `Route::{Albums,Album,Artists,Artist,Genres,Genre}` (unit
variants; the selected key lives in `State.open_album/open_artist/open_genre` as a display
**name** string). Grouping is done **app-side** from the flat `Vec<TrackView>`
(`group_albums`/`group_artists`/`group_genres`, first-seen fold). `TrackView` carries
`{id, title, artist, album, genre, duration_secs}` — **no year, no path, no cover bytes**.
Cover art is **per-track** via `host.art(id)`; "album cover" = one member track's id.
There is **no playlist** UI/command/State. The responsive grid already exists
(`albums.rs` reads `cx.layout_size` → `grid_layout`). The shared track row
(`detail_track_row`) and `cover()` helper are reused across pages.

Gap vs. the original: the **Artist page is just a flat track list** (the original has a
circular avatar + "Top Songs" + a "Discography" of albums); album/genre headers lack the
duration metadata and the shared hero grammar; and cards/heros are hand-rolled per page
rather than shared.

## Approach: shared components (mirrors the original)

Refactor the three pages onto shared primitives, then make each page thin (supply data +
sections). This matches the original's `MediaCard`/`DetailPageLayout`/`TrackList` reuse.

- **`views/media_card.rs`** — one card primitive: artwork tile (square `radius::MD` for
  albums, circular for artists) with a hover play-overlay, a truncating title, an optional
  muted subtitle, `role("button")`/`label`/`focusable`, click → a route. Album and Artist
  variants are thin wrappers; a Playlist variant is added by the later playlists project.
  Replaces `album_card`/`artist_card`.
- **`views/detail_page.rs`** — the shared hero: `detail_hero(artwork, eyebrow, title,
  subtitle, meta, actions)` laid out as a `row().flex_wrap().gap(space::XL)` (artwork
  left, text right) that reflows to stacked on a narrow pane; eyebrow in `type_::SECTION`
  (uppercased), title `type_::DISPLAY`, subtitle `type_::CARD_TITLE`, meta `type_::CAPTION`,
  then an actions row. Artwork is a square cover, a circular avatar, or a small icon tile
  (genre). Album/Artist/Genre detail headers all call this.
- **`views/grid.rs`** — `responsive_grid(width, min_card, gap) -> columns` + a layout
  helper, factored out of today's `albums.rs::grid_layout`. Reused by every browse page and
  the artist discography; reflows ~2→6 columns with the pane width (the original's
  breakpoints).
- Reuse the existing `detail_track_row`, `cover()`, and the `fmt_dur` helper as-is. Add one
  shared `play_all_pill(ids, p)` (the `radius::FULL` primary pill) used by every detail
  header.

Rejected: keep per-page bespoke code (what we have — the user asked to reuse components);
and a backend album/artist projection (unneeded — app-side grouping already works and
keeps this a pure-UI project).

## The three pages

### Album
- **Browse:** responsive grid of album cards (square cover + title + artist subtitle).
- **Detail:** shared hero — square cover, "ALBUM" eyebrow, album title, artist subtitle,
  metadata **"{n} tracks · {duration}"** (duration = sum of member `duration_secs`,
  formatted `H h M m` / `M min`), then the Play-All pill, then the album's **numbered**
  track list (`detail_track_row`).

### Artist (the main upgrade)
- **Browse:** responsive grid of **circular** artist cards (avatar + name + "{n} tracks").
- **Detail:** **circular avatar** hero (first-album cover in a circle; a blurred/darkened
  fallback is best-effort subject to Rune blur support — otherwise a plain circular cover),
  "ARTIST" eyebrow, name, metadata **"{n} albums · {n} tracks"**, Play-All pill. Body, in
  order:
  1. **Top Songs** — a `type_::HEADING` section with the artist's first **5** tracks in
     album-art track rows, and a **"Show more / Show less"** text toggle (state flag) that
     expands to the full list.
  2. **Discography** — a `type_::HEADING` section: the artist's tracks grouped by album
     (new pure `albums_of_artist(library, artist)` fold) into an album-card **responsive
     grid**; clicking an album card → the Album detail page. *(Grid only for v1; the
     original's grid/list toggle is a documented fast-follow.)*

### Genre (lowest divergence)
- **Browse:** keep the name **list** (genres have no art), rows = name + "{n} tracks".
- **Detail:** align to the shared hero using a small **icon tile** (a music/disc glyph in a
  `radius::MD` muted tile) in place of artwork — "GENRE" eyebrow, name, "{n} tracks ·
  {duration}", Play-All pill — then the genre's track list (album-art rows). *(The
  original's in-page search on the genre track list is a fast-follow, not v1.)*

## Data, navigation, state

- **All app-side, no backend/core change.** Reuse `group_albums`/`group_artists`; add a
  pure `albums_of_artist(library, artist) -> Vec<Album>` (filter the library to that
  artist, then the same album fold). Duration = sum of member `duration_secs`.
- **State:** add `artist_top_expanded: bool` (the Top-Songs show-more). Keep
  `open_album/open_artist/open_genre` (name keys) and the existing routes.
- **Navigation / back:** keep the current parent-route back buttons (no history stack —
  simple and already working; a real back-stack is a documented follow-up). Discography
  album cards navigate `Route::Album` + `open_album`.

## Deferred (documented, not in this project)

Year metadata (needs a `.soulidx` `date` field → catalog → backend projection, the
genres-feature shape); playlists (separate project); discography grid/list toggle; genre
in-page search; a navigation history/back-stack; the artist blurred-album avatar fallback
if Rune blur proves impractical.

## Testing (TDD)

- **Pure fns:** `albums_of_artist` groups only that artist's albums in first-seen order;
  duration-sum/format helper; reuse the existing grouping tests.
- **Render tests:** the Artist detail shows both a Top-Songs section and a Discography
  grid; "Show more" toggles `artist_top_expanded` and expands the list; an album card in
  the discography navigates to `Route::Album` with the right `open_album`; each detail
  header renders its eyebrow/title/meta; the shared `media_card` renders square vs circular.
- **i18n:** new `t!` keys (artist.topSongs, artist.showMore/showLess, album/artist/genre
  meta, etc.) in **all 6 locales**; **add `albums.rs`/`artists.rs`/`genres.rs`/`home.rs`
  and the new component files to the `views_have_no_hardcoded_ui_strings` scan list** (it
  currently checks only 5 files — a real coverage gap).
- Full suite green (app nightly + host + backend), clippy + fmt clean, CI matrix.

## Risks

- **Rune circular-artwork + blur support:** the circular avatar is low-risk (clip to a
  circle); the blurred-album fallback may not have a cheap Rune primitive — fall back to a
  plain circular cover + a muted overlay if so (documented, not blocking).
- **Responsive grid parity:** the reflow already works via `cx.layout_size`; the shared
  `grid.rs` just generalizes it — verify the first-frame fallback width still looks right.
- **Refactor blast radius:** moving `album_card`/`artist_card` into shared components
  touches three existing pages; covered by the render tests + `--snap` visual checks.
