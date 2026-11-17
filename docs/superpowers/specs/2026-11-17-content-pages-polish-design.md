# Content-pages polish — back-stack, Discography toggle, Genre search — design

Date: 2026-11-17
Status: approved direction, pre-implementation

## Goal

Finish the Rune head's Album/Artist/Genre pages with three deferred, **app/rune-only**
quality-of-life features, matching the original soul-player's behaviour:

1. **Navigation back-stack** — the detail-page back button returns to wherever you came
   from, not a hardcoded parent page.
2. **Discography grid/list toggle** — the Artist page's Discography can switch between the
   album-card grid (default) and a compact row list.
3. **Genre in-page search** — a search box on the Genre detail that filters its track list.

No `soul-audio-core`, backend, or host change — purely `app/rune`. Year metadata and
playlists remain separate, out of scope here.

## Current state (what exists)

Each detail page (`views/{albums,artists,genres}.rs`) has a `back_button(p)` that
**hardcodes the parent route** (`album_detail` back → `Route::Albums`, etc.). So an Album
opened from an Artist's Discography card (`artists.rs:250` sets `Route::Album`) still backs
out to Albums, losing the Artist you came from. Navigation is a flat `State.route` +
`open_album`/`open_artist`/`open_genre` (name keys); there is no history. The Discography
renders a `media_card` grid only. The Genre detail renders `detail_track_row` over
`genre.ids` with no filter. Sidebar nav items set `route` directly (`sidebar.rs`).

## Feature 1 — Navigation back-stack

### Data
Add to `State`:
- `nav_history: Vec<NavEntry>` — the locations behind the current one.

New type (in `main.rs`, beside `Route`):
```rust
#[derive(Clone, PartialEq)]
pub(crate) struct NavEntry {
    pub route: Route,
    pub open_album: Option<String>,
    pub open_artist: Option<String>,
    pub open_genre: Option<String>,
}
```

### Navigation helpers (in `main.rs`)
- `fn here(s: &State) -> NavEntry` — snapshots the current location.
- `fn apply(s: &mut State, e: NavEntry)` — sets `route`/`open_*` from an entry.
- `pub(crate) fn navigate_to(s: &mut State, route: Route, album: Option<String>, artist:
  Option<String>, genre: Option<String>)` — pushes `here(s)` onto `nav_history`, then sets
  the new location. Every **detail-opening** interaction goes through it: the album card,
  artist card, genre row, the Discography album card, and the now-playing → album link.
- `pub(crate) fn go_back(s: &mut State)` — pops `nav_history`; if `Some(prev)`, `apply`s it;
  if empty, falls back to the current detail's parent (`Album|Albums → Albums`,
  `Artist → Artists`, `Genre → Genres`, else `Home`) so back is never a dead end. The
  three `back_button`s call this instead of hardcoding a route.
- **Sidebar nav** (`nav_item` press) clears `nav_history` and sets the route directly — a
  top-level nav is a fresh context, so the stack can't grow unbounded and "back" only
  matters within a detail drill-down.

### Behaviour
Artists → (open artist, pushes Artists) → Discography card → (open album, pushes the
artist) → Back → the artist → Back → Artists. A bounded, intuitive stack. No forward button
(deferred). `nav_history` is ephemeral (not persisted).

## Feature 2 — Discography grid/list toggle

### Data
Add `State.discography_list: bool` (default `false` = grid).

### UI
In `artist_detail`'s Discography section header (`views/artists.rs`), add a two-button
grid/list toggle (`views::icons` grid + list glyphs) keyed `disc-toggle-grid` /
`disc-toggle-list`, the active one lit (`fx::SELECTED`), each flipping `discography_list`.
- **Grid** (default): today's `media_card` responsive grid, unchanged.
- **List**: the albums as compact rows — a small square cover thumb (≈48 pt), the album
  name, and "{n} tracks · {duration}" muted, `role("button")`, each keyed `artist-disc-row-{i}`
  and opening the Album page via `navigate_to`. A thin `album_row(...)` helper in
  `artists.rs` (or shared), reusing `cover`/`fmt_total_dur`.

The toggle is Artist-page-local; it does not affect the browse grids. State is ephemeral
(resets to grid each launch).

## Feature 3 — Genre in-page search

### Data
Add `State.genre_search: String` (cleared when a genre opens, in the genre row's
`on_press`).

### UI
Above the Genre detail track list (`views/genres.rs`), a search input (styled like the
library search; keyed `genre-search`) bound to `genre_search`, with a clear affordance when
non-empty. The track list filters live: keep only `genre.ids` whose `TrackView` matches the
lowercased query via the existing `track_matches` helper (title/artist/album substring,
re-exported from `views::library`). Empty query shows all; a no-match shows a muted
"No matches" line. The Play-All / header counts reflect the **unfiltered** genre (the
filter is a view over the list, not a redefinition of the genre).

## Data, navigation, state — summary

All app-side; no backend/core change. New `State` fields: `nav_history: Vec<NavEntry>`,
`discography_list: bool`, `genre_search: String` (all ephemeral, defaulted in `State::new`).
New shared helper `track_matches` must be made `pub(crate)` in `views/library.rs` (today
it's private).

## Testing (TDD)

- **Pure fns / logic:** `go_back` pops to the pushed entry and falls back to the parent when
  empty; the genre filter keeps only matching ids.
- **Render tests** (`main.rs` harness): Artist → open album via Discography → Back returns
  to the **artist** (route + `open_artist`), not Albums; the Discography toggle flips
  `discography_list` and a list row opens the album; typing into `genre-search` hides
  non-matching rows and a cleared query restores them.
- **i18n:** new `t!` keys (e.g. `genre.searchPlaceholder`, `genre.noMatches`,
  `artist.viewGrid`/`artist.viewList` a11y labels) in all 6 locales; the touched view files
  stay covered by `views_have_no_hardcoded_ui_strings`.
- Full `app/rune` suite green, clippy `-D warnings` + fmt clean, CI matrix.

## Risks / notes

- **Back-stack centralization:** routing today is scattered (`st.route = …` in many
  `on_press`). The risk is missing a call site; the design routes **only the five
  detail-opening interactions** through `navigate_to` and leaves browse/settings/sidebar as
  direct sets — a small, enumerable surface. Render tests cover the main drill-down.
- **Genre search vs library search:** kept as a **separate** `genre_search` field (not the
  library's `State.search`) so the two never cross-contaminate.
- **Toggle/search are ephemeral:** no persistence (YAGNI); they reset each launch.
