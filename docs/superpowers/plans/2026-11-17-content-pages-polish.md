# Content-pages Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the Rune head's Album/Artist/Genre pages with a navigation back-stack, a Discography grid/list toggle, and genre in-page search.

**Architecture:** All changes live in `app/rune` (no backend/core). A small set of nav helpers in `main.rs` centralise the five "open a detail" interactions and the three back buttons; a `State.discography_list` flag + a two-icon toggle switch the Artist page's Discography between grid and a row list; a `rune::TextEdit` field drives a self-contained search box on the Genre detail that filters its track list.

**Tech Stack:** Rust (edition 2024, **nightly-2026-11-10** for `app/rune` only — pinned in `app/rune/rust-toolchain.toml`, selected automatically when the shell CWD is `app/rune`), the `rune` immediate-mode GUI crate (incl. its `text_input` widget + `TextEdit`), `rust-i18n` (`t!`).

## Working Context
- **Branch / worktree:** `feat/content-pages-polish` in `D:/dev/soulaudio/soul-player-ng/.claude/worktrees/output-device-ui` (already created off `origin/main`).
- **Crate:** `soul-player-rune` (binary). Run every cargo command **from `app/rune/`**. Test filter: `cargo test <name>`.
- **Spec:** `docs/superpowers/specs/2026-11-17-content-pages-polish-design.md`.

## Global Constraints
- Commit messages: NO "Claude"/"Anthropic"/"AI"/"Co-Authored-By"/attribution (commit-msg hook rejects). Conventional-commit style. `--no-verify` authorized only if a hook misfires.
- `#![forbid(unsafe_code)]` stays atop every view module.
- All user-facing text via `t!("…")` — never a bare `text("word")`/`.label("word")` (glyphs like `"←"` are exempt). New keys go in `app/rune/locales/app.yml` in ALL 6 locales (`en, de, es, zh, cs, ja`), exact format (unindented `key:`, then 2-space `lang: 'value'`, single-quoted). `every_key_is_translated_in_all_shipped_locales` fails until every cell is filled.
- Each task ends with the full `app/rune` suite green + `cargo clippy --all-targets -- -D warnings` + `cargo fmt --all -- --check` clean. Delete imports a refactor orphans.
- No backend/core edits. TDD: failing test first, watch it fail for the right reason, minimal implementation, watch it pass.

## File Structure
**Modify:**
- `app/rune/src/main.rs` — add `NavEntry` type; `State.nav_history`/`discography_list`/`genre_search` fields + `State::new` defaults; nav helpers `here`/`navigate_to`/`go_back`; render tests.
- `app/rune/src/views/albums.rs` — album card + back button via the nav helpers.
- `app/rune/src/views/artists.rs` — artist card + Discography card + back button via nav helpers; the grid/list toggle + `album_row` list mode.
- `app/rune/src/views/genres.rs` — genre row + back button via nav helpers; the search box + filtered list.
- `app/rune/src/views/player.rs` — now-playing→album link via the nav helper.
- `app/rune/src/views/sidebar.rs` — nav item press clears history + search.
- `app/rune/src/views/library.rs` — make `track_matches` `pub(crate)`.
- `app/rune/src/views/icons.rs` — add `GRID` + `LIST` lucide icons.
- `app/rune/locales/app.yml` — new `artist.viewGrid`/`artist.viewList`/`genre.searchPlaceholder`/`genre.noMatches` keys.

---

### Task 1: Navigation back-stack

**Files:**
- Modify: `app/rune/src/main.rs` (Route-adjacent `NavEntry`; State fields; `State::new`; helpers; dispatch unchanged), `albums.rs`, `artists.rs` (card + disc card + back), `genres.rs` (row + back), `player.rs` (now-playing), `sidebar.rs` (nav item)
- Test: render tests in `main.rs`

**Interfaces:**
- Produces: `pub(crate) struct NavEntry { route: Route, open_album: Option<String>, open_artist: Option<String>, open_genre: Option<String> }` (`#[derive(Clone, PartialEq)]`); `State.nav_history: Vec<NavEntry>`; `pub(crate) fn navigate_to(s: &mut State, route: Route, open_album: Option<String>, open_artist: Option<String>, open_genre: Option<String>)`; `pub(crate) fn go_back(s: &mut State)`.
- Consumes: existing `Route` (Copy), `State.open_album/open_artist/open_genre`, `State.genre_search` (added in Task 3 — for Task 1, omit the `genre_search` reset lines; Task 3 adds them).

- [ ] **Step 1: Write the failing render test.** In `app/rune/src/main.rs` tests, add:

```rust
    #[test]
    fn back_returns_to_where_you_came_from() {
        // Artist → (Discography) → Album → Back must return to the ARTIST, not the
        // hardcoded Albums parent.
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            artist_album_track(1, "Bach", "Cello Suites"),
            artist_album_track(2, "Bach", "Goldberg"),
        ];
        s.route = Route::Artists;
        let env = Env { system_dark: true, scale: 2.0 };
        let mut rt = Runtime::new(s, view, (1400.0, 900.0), env);
        rt.frame(0.0);
        click(&mut rt, "card-text"); // open the artist (Bach) — pushes Artists
        assert!(matches!(rt.state.route, Route::Artist));
        rt.frame(0.0);
        rt.frame(0.0);
        click(&mut rt, "card-text"); // first Discography album — pushes the artist
        assert!(matches!(rt.state.route, Route::Album), "a disc card opens the album");
        click(&mut rt, "album-back"); // Back
        assert!(matches!(rt.state.route, Route::Artist), "back returns to the artist");
        assert_eq!(rt.state.open_artist.as_deref(), Some("Bach"));
        click(&mut rt, "artist-back"); // Back again
        assert!(matches!(rt.state.route, Route::Artists), "and then to the Artists list");
    }
```

(`artist_album_track` + `click` already exist in the test module.)

- [ ] **Step 2: Run it, watch it fail.** From `app/rune/`: `cargo test back_returns_to_where_you_came_from`
Expected: FAIL — `album-back` currently hardcodes `Route::Albums`, so the first Back lands on Albums, not the artist.

- [ ] **Step 3: Add `NavEntry` + State fields + helpers.** In `main.rs`, right after the `Route` enum, add:

```rust
/// One entry in the navigation back-stack: a full location (route + which
/// album/artist/genre is open). Pushed by `navigate_to`, restored by `go_back`.
#[derive(Clone, PartialEq)]
pub(crate) struct NavEntry {
    pub route: Route,
    pub open_album: Option<String>,
    pub open_artist: Option<String>,
    pub open_genre: Option<String>,
}

/// The current location as a `NavEntry`.
fn here(s: &State) -> NavEntry {
    NavEntry {
        route: s.route,
        open_album: s.open_album.clone(),
        open_artist: s.open_artist.clone(),
        open_genre: s.open_genre.clone(),
    }
}

/// Open a detail location, remembering where we were so `go_back` can return. The
/// five "open a detail" interactions (album/artist/genre cards, the Discography
/// card, and the now-playing → album link) route through this.
pub(crate) fn navigate_to(
    s: &mut State,
    route: Route,
    open_album: Option<String>,
    open_artist: Option<String>,
    open_genre: Option<String>,
) {
    s.nav_history.push(here(s));
    s.route = route;
    s.open_album = open_album;
    s.open_artist = open_artist;
    s.open_genre = open_genre;
    s.mobile_show_content = true;
}

/// The detail back button: pop to the previous location, or — if the stack is
/// empty — fall back to the current page's browse parent so back is never a dead
/// end.
pub(crate) fn go_back(s: &mut State) {
    if let Some(prev) = s.nav_history.pop() {
        s.route = prev.route;
        s.open_album = prev.open_album;
        s.open_artist = prev.open_artist;
        s.open_genre = prev.open_genre;
    } else {
        s.route = match s.route {
            Route::Album | Route::Albums => Route::Albums,
            Route::Artist | Route::Artists => Route::Artists,
            Route::Genre | Route::Genres => Route::Genres,
            _ => Route::Home,
        };
    }
}
```

In the `State` struct, after `open_genre: Option<String>,`, add:

```rust
    /// The navigation back-stack (ephemeral): locations behind the current one, so
    /// a detail's back button returns to where you came from rather than a
    /// hardcoded parent. Pushed by [`navigate_to`], popped by [`go_back`]; cleared
    /// by a top-level sidebar nav.
    nav_history: Vec<NavEntry>,
```

In `State::new`, after `open_genre: None,`, add:

```rust
            nav_history: Vec::new(),
```

- [ ] **Step 4: Route the five detail-opening interactions + three back buttons + sidebar nav through the helpers.**

In `app/rune/src/views/albums.rs` — `album_card`'s `on_press` closure body becomes:

```rust
        move |st: &mut State| {
            crate::navigate_to(st, crate::Route::Album, Some(name.clone()), None, None);
        },
```

and `back_button`'s `.on_press(...)`:

```rust
        .on_press(crate::go_back)
```

In `app/rune/src/views/artists.rs` — `artist_card`'s `on_press`:

```rust
        move |st: &mut State| {
            crate::navigate_to(st, crate::Route::Artist, None, Some(name.clone()), None);
            st.artist_top_expanded = false;
        },
```

the Discography card's `on_press`:

```rust
                move |st: &mut State| {
                    crate::navigate_to(st, crate::Route::Album, Some(name.clone()), None, None);
                },
```

and `back_button`'s `.on_press(...)`:

```rust
        .on_press(crate::go_back)
```

In `app/rune/src/views/genres.rs` — `genre_row`'s `on_press`:

```rust
        .on_press(move |st: &mut State| {
            crate::navigate_to(st, crate::Route::Genre, None, None, Some(name.clone()));
        })
```

and `back_button`'s `.on_press(...)`:

```rust
        .on_press(crate::go_back)
```

In `app/rune/src/views/player.rs` — the now-playing→album `on_press` (keeps the `if let Some(a)` guard):

```rust
            .on_press(move |st: &mut State| {
                if let Some(a) = &album {
                    crate::navigate_to(st, crate::Route::Album, Some(a.clone()), None, None);
                }
            })
```

In `app/rune/src/views/sidebar.rs` — `nav_item`'s `on_press` clears the stack (fresh top-level context):

```rust
            .on_press(move |s: &mut State| {
                s.nav_history.clear();
                s.route = route;
                s.mobile_show_content = true;
            });
```

- [ ] **Step 5: Run the test, watch it pass.** From `app/rune/`: `cargo test back_returns_to_where_you_came_from`
Expected: PASS. Also run `cargo test` for the existing album/artist/genre nav tests (`clicking_an_artist_card_opens_the_detail_page`, `clicking_the_now_playing_track_opens_its_album`, `a_discography_card_opens_the_album`, etc.) — they still pass (the end state is unchanged; only history is now recorded).

- [ ] **Step 6: Add a pure go_back fallback test.** Add:

```rust
    #[test]
    fn back_falls_back_to_the_parent_with_an_empty_stack() {
        let mut s = State::new(PlayerHandle::offline());
        s.route = Route::Album; // reached without history (e.g. deep link)
        go_back(&mut s);
        assert!(matches!(s.route, Route::Albums), "album → albums");
        s.route = Route::Genre;
        go_back(&mut s);
        assert!(matches!(s.route, Route::Genres), "genre → genres");
    }
```

Run: `cargo test back_falls_back_to_the_parent` — PASS.

- [ ] **Step 7: Suite + lints.** From `app/rune/`: `cargo test` then `cargo clippy --all-targets -- -D warnings` then `cargo fmt --all`. All green; clippy clean (delete any orphaned import).

- [ ] **Step 8: Commit.**

```bash
git add app/rune/src/main.rs app/rune/src/views/albums.rs app/rune/src/views/artists.rs app/rune/src/views/genres.rs app/rune/src/views/player.rs app/rune/src/views/sidebar.rs
git commit -m "feat(nav): navigation back-stack so detail back returns to where you came from"
```

---

### Task 2: Discography grid/list toggle

**Files:**
- Modify: `app/rune/src/main.rs` (State field + default), `app/rune/src/views/icons.rs` (GRID + LIST icons), `app/rune/src/views/artists.rs` (toggle + `album_row` list mode), `app/rune/locales/app.yml`
- Test: render test in `main.rs`

**Interfaces:**
- Consumes: `navigate_to` (Task 1); `crate::views::player::cover`; `crate::views::albums::fmt_total_dur`; `State.discography_list`.
- Produces: `State.discography_list: bool`; `icons::GRID`, `icons::LIST`.

- [ ] **Step 1: Add the two lucide icons.** In `app/rune/src/views/icons.rs`, after the `MUSIC` icon (or with the other `stroke_icon!` statics), add:

```rust
/// lucide `layout-grid` — the Discography grid-view toggle.
pub static GRID: Icon = stroke_icon!(
    "grid",
    r#"<rect width="7" height="7" x="3" y="3" rx="1"/><rect width="7" height="7" x="14" y="3" rx="1"/><rect width="7" height="7" x="14" y="14" rx="1"/><rect width="7" height="7" x="3" y="14" rx="1"/>"#
);
/// lucide `list` — the Discography list-view toggle.
pub static LIST: Icon = stroke_icon!(
    "list",
    r#"<line x1="8" x2="21" y1="6" y2="6"/><line x1="8" x2="21" y1="12" y2="12"/><line x1="8" x2="21" y1="18" y2="18"/><line x1="3" x2="3.01" y1="6" y2="6"/><line x1="3" x2="3.01" y1="12" y2="12"/><line x1="3" x2="3.01" y1="18" y2="18"/>"#
);
```

- [ ] **Step 2: Add the State field.** In `main.rs` `State`, after `nav_history: Vec<NavEntry>,`:

```rust
    /// Artist-page Discography view mode (ephemeral): `false` = album-card grid
    /// (default), `true` = compact row list. Toggled by the grid/list buttons.
    discography_list: bool,
```

In `State::new`, after `nav_history: Vec::new(),`:

```rust
            discography_list: false,
```

- [ ] **Step 3: Add the 4 locale keys (2 here, 2 in Task 3).** In `app/rune/locales/app.yml`, after the `artist.showLess` block, add:

```yaml
artist.viewGrid:
  en: 'Grid view'
  de: 'Rasteransicht'
  es: 'Vista de cuadrícula'
  zh: '网格视图'
  cs: 'Mřížka'
  ja: 'グリッド表示'
artist.viewList:
  en: 'List view'
  de: 'Listenansicht'
  es: 'Vista de lista'
  zh: '列表视图'
  cs: 'Seznam'
  ja: 'リスト表示'
```

- [ ] **Step 4: Write the failing render test.** In `main.rs` tests:

```rust
    #[test]
    fn discography_grid_list_toggle_switches_and_lists_navigate() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            artist_album_track(1, "Bach", "Cello Suites"),
            artist_album_track(2, "Bach", "Goldberg"),
        ];
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env { system_dark: true, scale: 2.0 };
        let mut rt = Runtime::new(s, view, (1400.0, 900.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        // Default is grid: cards present, no rows.
        assert!(rt.find("artist-disc-0").is_some(), "grid cards by default");
        assert!(rt.find("artist-disc-row-0").is_none());
        // Switch to list.
        click(&mut rt, "disc-toggle-list");
        assert!(rt.state.discography_list, "toggle flips to list");
        rt.frame(0.0);
        assert!(rt.find("artist-disc-row-0").is_some(), "list rows render");
        assert!(rt.find("artist-disc-0").is_none(), "grid cards gone");
        // A list row opens the album (through the back-stack).
        click(&mut rt, "artist-disc-row-0");
        assert!(matches!(rt.state.route, Route::Album), "a list row opens the album");
        assert_eq!(rt.state.open_album.as_deref(), Some("Cello Suites"));
    }
```

- [ ] **Step 5: Run it, watch it fail.** From `app/rune/`: `cargo test discography_grid_list_toggle` — FAIL (`disc-toggle-list`/`artist-disc-row-0` don't exist).

- [ ] **Step 6: Implement the toggle + list mode.** In `app/rune/src/views/artists.rs`, add an `album_row` helper (near the bottom, above `#[cfg(test)]`):

```rust
/// One Discography row (list mode): a small square cover, the album name, and
/// "{n} tracks · {duration}". Opens the album via the back-stack.
fn album_row(s: &State, p: &Palette, i: usize, alb: &Album, scale: f32) -> El {
    let total: f64 = alb
        .ids
        .iter()
        .filter_map(|id| s.library.iter().find(|t| t.id == *id))
        .map(|t| t.duration_secs)
        .sum();
    let meta = t!(
        "detail.tracksAndDuration",
        count => alb.ids.len(),
        dur => crate::views::albums::fmt_total_dur(total)
    )
    .to_string();
    let name = alb.name.clone();
    row()
        .key(format!("artist-disc-row-{i}"))
        .role("button")
        .label(alb.name.clone())
        .focusable(true)
        .w_full()
        .gap(space::MD)
        .center_y()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(cover(s, Some(alb.cover_id), 48.0, radius::SM, p, scale))
        .child(
            col()
                .gap(space::XXS)
                .min_w(0.0)
                .grow(1.0)
                .child(text(alb.name.clone()).font(type_::ITEM).color(p.fg).truncate())
                .child(text(meta).font(type_::CAPTION).color(p.text_secondary).truncate()),
        )
        .on_press(move |st: &mut State| {
            crate::navigate_to(st, crate::Route::Album, Some(name.clone()), None, None);
        })
}
```

Add the import `use crate::views::player::cover;` near the top of `artists.rs` (it's not currently imported there — the hero uses it via `detail_page`, so add it).

Then in `artist_detail`, replace the Discography section (the `let mut disc = col()…` through `disc = disc.child(grid_el);` block) with a header-with-toggle + a body that switches on `s.discography_list`:

```rust
    // Discography: a grid/list toggle in the header, then either the responsive
    // card grid (default) or a compact row list.
    let pane = cx
        .layout_size("artist-discography")
        .map(|[w, _]| w)
        .unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;
    let toggle = |key: &str, glyph: &'static rune::Icon, label: String, active: bool| {
        let want_list = key == "disc-toggle-list";
        el().key(key.to_string())
            .role("button")
            .label(label)
            .focusable(true)
            .px(space::SNUG)
            .py(space::XS)
            .radius(radius::SM)
            .cursor(Cursor::Pointer)
            .fill(if active { p.fg.alpha(fx::SELECTED) } else { Color::TRANSPARENT })
            .hover(p.fg.alpha(fx::HOVER))
            .child(icon(glyph, 18.0).color(if active { p.fg } else { p.text_secondary }))
            .on_press(move |st: &mut State| st.discography_list = want_list)
    };
    let header = row()
        .key("artist-disc-header")
        .center_y()
        .child(
            text(t!("artist.discography").to_string())
                .font(type_::HEADING)
                .color(p.fg)
                .grow(1.0),
        )
        .child(toggle("disc-toggle-grid", &icons::GRID, t!("artist.viewGrid").to_string(), !s.discography_list))
        .child(toggle("disc-toggle-list", &icons::LIST, t!("artist.viewList").to_string(), s.discography_list));
    let mut disc = col().key("artist-discography").gap(space::SM).child(header);
    if !discography.is_empty() {
        if s.discography_list {
            let mut list = col().key("artist-disc-list").role("list").gap(space::XXS);
            for (i, alb) in discography.iter().enumerate() {
                list = list.child(album_row(s, p, i, alb, scale));
            }
            disc = disc.child(list);
        } else {
            let mut grid_el = row()
                .key("artist-disc-grid")
                .role("grid")
                .flex_wrap()
                .gaps(space::MD, space::GUTTER);
            for (slot, alb) in discography.iter().enumerate() {
                let name = alb.name.clone();
                grid_el = grid_el.child(media_card(
                    s,
                    p,
                    CardSpec {
                        key: format!("artist-disc-{slot}"),
                        label: alb.name.clone(),
                        title: alb.name.clone(),
                        subtitle: None,
                        cover_id: alb.cover_id,
                        circular: false,
                        play_ids: alb.ids.clone(),
                    },
                    card_w,
                    scale,
                    move |st: &mut State| {
                        crate::navigate_to(st, crate::Route::Album, Some(name.clone()), None, None);
                    },
                ));
            }
            disc = disc.child(grid_el);
        }
    }
```

Add `icon` and `Color` to the `rune::{…}` import in `artists.rs` if not already present (the toggle uses `icon(...)` and `Color::TRANSPARENT`), and `use crate::views::icons;`. Reconcile against clippy in Step 8.

- [ ] **Step 7: Run it, watch it pass.** From `app/rune/`: `cargo test discography_grid_list_toggle every_key_is_translated` — PASS.

- [ ] **Step 8: Suite + lints.** `cargo test` then `cargo clippy --all-targets -- -D warnings` then `cargo fmt --all` — clean. Confirm `the_artist_detail_has_top_songs_and_discography` still passes (grid is still the default).

- [ ] **Step 9: Commit.**

```bash
git add app/rune/src/main.rs app/rune/src/views/artists.rs app/rune/src/views/icons.rs app/rune/locales/app.yml
git commit -m "feat(artist): grid/list toggle for the Discography section"
```

---

### Task 3: Genre in-page search

**Files:**
- Modify: `app/rune/src/main.rs` (State field + default + reset-on-open; nav helpers reset search), `app/rune/src/views/library.rs` (`track_matches` → `pub(crate)`), `app/rune/src/views/genres.rs` (search box + filter), `app/rune/locales/app.yml`
- Test: filter + render tests

**Interfaces:**
- Consumes: `crate::views::library::track_matches` (made `pub(crate)`); `rune::{TextEdit, text_input}`; `State.genre_search`.
- Produces: `State.genre_search: rune::TextEdit`.

- [ ] **Step 1: Make `track_matches` shareable.** In `app/rune/src/views/library.rs`, change `fn track_matches(` to `pub(crate) fn track_matches(`.

- [ ] **Step 2: Add the State field + resets.** In `main.rs` `State`, after `discography_list: bool,`:

```rust
    /// The Genre-detail in-page search box (ephemeral): filters the open genre's
    /// track list. Separate from the library search so the two never cross over;
    /// reset whenever we navigate (see [`navigate_to`]/[`go_back`]).
    genre_search: rune::TextEdit,
```

In `State::new`, after `discography_list: false,`:

```rust
            genre_search: rune::TextEdit::new(""),
```

In `navigate_to` (Task 1), before the final `}`, add:

```rust
    s.genre_search = rune::TextEdit::new("");
```

In `go_back` (Task 1), as the first line of the function body, add:

```rust
    s.genre_search = rune::TextEdit::new("");
```

In `sidebar.rs` `nav_item`'s `on_press` (Task 1), add after `s.nav_history.clear();`:

```rust
                s.genre_search = rune::TextEdit::new("");
```

- [ ] **Step 3: Add the 2 locale keys.** In `app/rune/locales/app.yml`, after the `artist.viewList` block (Task 2), add:

```yaml
genre.searchPlaceholder:
  en: 'Search this genre'
  de: 'In diesem Genre suchen'
  es: 'Buscar en este género'
  zh: '搜索此流派'
  cs: 'Hledat v žánru'
  ja: 'このジャンルを検索'
genre.noMatches:
  en: 'No matches'
  de: 'Keine Treffer'
  es: 'Sin coincidencias'
  zh: '无匹配项'
  cs: 'Žádné shody'
  ja: '一致なし'
```

- [ ] **Step 4: Write the failing render test.** In `main.rs` tests:

```rust
    #[test]
    fn genre_in_page_search_filters_the_track_list() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        // Two tracks with distinct titles in the one genre.
        s.library = vec![
            soul_player_host::TrackView { id: 1, title: Some("Blue".into()), artist: Some("A".into()), album: Some("Alb".into()), genre: Some("Jazz".into()), duration_secs: 1.0 },
            soul_player_host::TrackView { id: 2, title: Some("Green".into()), artist: Some("A".into()), album: Some("Alb".into()), genre: Some("Jazz".into()), duration_secs: 1.0 },
        ];
        s.route = Route::Genre;
        s.open_genre = Some("Jazz".into());
        let env = Env { system_dark: true, scale: 2.0 };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("genre-search").is_some(), "the search box renders");
        assert!(rt.find("album-track-0").is_some() && rt.find("album-track-1").is_some(), "both tracks show");
        // Type a query that matches only "Blue".
        rt.update(|st: &mut State, _tx: &mut Tx| st.genre_search = rune::TextEdit::new("blue"));
        rt.frame(0.0);
        assert!(rt.find("album-track-0").is_some(), "the match stays");
        assert!(rt.find("album-track-1").is_none(), "the non-match is filtered out");
    }
```

- [ ] **Step 5: Run it, watch it fail.** From `app/rune/`: `cargo test genre_in_page_search` — FAIL (`genre-search` doesn't exist; both tracks always show).

- [ ] **Step 6: Implement the search box + filter.** In `app/rune/src/views/genres.rs`:
  - Add imports: `use rune::{text_input};` folded into the existing `rune::{…}` use; nothing else new.
  - Replace the track-list build (the `let mut list = col().key("genre-tracks")…` loop) with a filtered build + a search box, and insert the search box above the list in the content column. Concretely, after computing `meta`/`hero`, replace the list loop with:

```rust
    let q = s.genre_search.text.to_lowercase();
    let shown: Vec<u32> = genre
        .ids
        .iter()
        .copied()
        .filter(|id| {
            q.is_empty()
                || s.library
                    .iter()
                    .find(|t| t.id == *id)
                    .is_some_and(|t| crate::views::library::track_matches(t, &q))
        })
        .collect();
    let search = text_input(&s.genre_search, type_::ITEM)
        .key("genre-search")
        .label(t!("genre.searchPlaceholder").to_string())
        .w_full()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .color(p.fg)
        .on_edit(|st: &mut State| &mut st.genre_search);
    let mut list = col().key("genre-tracks").role("list").gap(space::XXS);
    if shown.is_empty() {
        list = list.child(
            text(t!("genre.noMatches").to_string())
                .font(type_::ITEM)
                .color(p.text_secondary)
                .px(space::MD)
                .py(space::SM),
        );
    } else {
        for (i, &id) in shown.iter().enumerate() {
            if let Some(t) = s.library.iter().find(|t| t.id == id) {
                list = list.child(detail_track_row(s.now_playing_id, s.playing, p, &shown, i, t));
            }
        }
    }
```

  Then in the returned content column, insert `.child(search)` between `.child(hero)` and `.child(list)`:

```rust
                .child(hero)
                .child(search)
                .child(list)
```

  Note: `detail_track_row` is now fed `&shown` (the filtered id list) so playing a filtered row starts a context over exactly the visible tracks (matching the library-search behaviour). The hero's "{n} tracks · {duration}" still counts the full `genre.ids` (unchanged) — the filter is a view over the list, not the genre.

- [ ] **Step 7: Run it, watch it pass.** From `app/rune/`: `cargo test genre_in_page_search every_key_is_translated` — PASS.

- [ ] **Step 8: Suite + lints.** `cargo test` then `cargo clippy --all-targets -- -D warnings` then `cargo fmt --all` — clean. Confirm `the_genre_detail_lists_tracks_with_play_all_and_back` still passes (empty query → all tracks).

- [ ] **Step 9: Commit.**

```bash
git add app/rune/src/main.rs app/rune/src/views/library.rs app/rune/src/views/genres.rs app/rune/locales/app.yml
git commit -m "feat(genre): in-page search that filters the genre track list"
```

---

## Self-Review (completed by plan author)

**Spec coverage:**
- Back-stack (`NavEntry`, `nav_history`, `navigate_to`/`go_back`, 5 detail-opening callers + 3 back buttons + sidebar clear) → Task 1. ✅
- Discography grid/list toggle (`discography_list`, two-icon toggle, `album_row` list) → Task 2. ✅
- Genre in-page search (`genre_search` TextEdit, `track_matches` pub(crate), `text_input` + filter, reset on nav) → Task 3. ✅
- i18n new keys in 6 locales (artist.viewGrid/viewList in Task 2; genre.searchPlaceholder/noMatches in Task 3); touched view files already covered by the hardcoded-string scan. ✅
- Ephemeral state (no persistence); separate genre_search field → Tasks 2/3. ✅
- Deferred (year, playlists, forward button, fuzzy search) untouched. ✅

**Type consistency:** `NavEntry` fields match in `here`/`navigate_to`/`go_back`; `navigate_to(s, route, open_album, open_artist, open_genre)` signature used identically by all five callers; `go_back` is `fn(&mut State)` passed to `.on_press`; `discography_list: bool` and `genre_search: rune::TextEdit` match struct ↔ `State::new` ↔ usage; `track_matches(&TrackView, &str) -> bool` consistent; `icons::GRID`/`icons::LIST` used in artists.rs. ✅

**Placeholders:** none — every step shows complete code; Task 1 explicitly notes the `genre_search` reset lines are added in Task 3 (the field doesn't exist until then), avoiding a forward reference that wouldn't compile.
