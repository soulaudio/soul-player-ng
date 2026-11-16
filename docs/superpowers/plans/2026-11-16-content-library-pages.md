# Content / Library Pages Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Upgrade the Rune head's Album, Artist, and Genre pages to look and behave like the original soul-player by extracting shared components (card, detail hero, responsive grid, Play-All pill) and giving the Artist page a circular avatar + Top Songs + Discography.

**Architecture:** Factor the three hand-rolled pages onto three new shared view modules — `views/grid.rs` (responsive column math), `views/media_card.rs` (the original's `MediaCard`), `views/detail_page.rs` (the original's `DetailPageLayout` hero + Play-All pill) — then make each page thin: supply data + sections. All grouping stays app-side from the flat `Vec<TrackView>`; no backend or core change.

**Tech Stack:** Rust (edition 2024, **nightly-2026-11-10** for `app/rune` only — the toolchain is pinned in `app/rune/rust-toolchain.toml` and selected automatically when the shell CWD is `app/rune`), the `rune` immediate-mode GUI crate, `rust-i18n` (`t!` macro) for all UI text.

## Working Context

- **Worktree / branch:** all work happens in `D:/dev/soulaudio/soul-player-ng/.claude/worktrees/output-device-ui` on branch `feat/content-pages` (already created off `origin/main`, spec committed as `a728c27`).
- **Crate:** `soul-player-rune` (binary, `src/main.rs`). Run every `cargo` command **from `app/rune/`** so rustup picks the pinned nightly. Test filter form: `cargo test <name>` (the test module lives in the bin crate).
- **Design spec:** `docs/superpowers/specs/2026-11-16-content-library-pages-design.md` (approved).

## Global Constraints

- **Commit messages:** NO "Claude", "Anthropic", "AI", "Co-Authored-By", or any attribution — the commit-msg hook rejects them. Conventional-commit style (`feat(views): …`, `refactor(views): …`).
- **`--no-verify` is authorized** for commits/pushes in this worktree if a hook misfires, but prefer letting hooks run.
- **`#![forbid(unsafe_code)]`** stays at the top of every view module (new files included).
- **All user-facing text goes through `t!("…")`** — never a bare `text("word")` or `.label("word")`. Glyphs (`"←"`, `"▶"`, `"⏸"`) are exempt (the scanner only flags runs of 3+ ASCII letters).
- **i18n is complete-or-red:** every new `t!` key must be added to `app/rune/locales/app.yml` in **all 6 shipped locales** — `en, de, es, zh, cs, ja` — in the exact format (unindented `key:`, then 2-space `lang: 'value'` lines, single-quoted values). The `every_key_is_translated_in_all_shipped_locales` test fails until every cell is filled.
- **Per-task green gate:** each task ends with the full `app/rune` test suite green plus `cargo clippy` and `cargo fmt --all` clean (run from `app/rune/`). Remove any import a refactor orphaned — clippy denies unused imports.
- **No backend/core edits.** This is a pure `app/rune` project.
- **TDD throughout:** write the failing test, watch it fail for the right reason, minimal implementation, watch it pass, commit.

---

## File Structure

**Create:**
- `app/rune/src/views/grid.rs` — `responsive_grid(pane_w, min_card, gap) -> (cols, card_w)` + `MIN_CARD`. The responsive column math, factored out of `albums.rs::grid_layout`.
- `app/rune/src/views/media_card.rs` — `CardSpec`, `card_play_command`, `media_card(...)`. The shared library card (square album / circular artist), hover play-overlay, title + optional subtitle, body-click → `on_open`.
- `app/rune/src/views/detail_page.rs` — `HeroArt`, `detail_hero(...)`, `play_all_pill(...)`, `DETAIL_COVER`. The shared detail header + primary pill.

**Modify:**
- `app/rune/src/views/mod.rs` — register the 3 new modules.
- `app/rune/src/views/albums.rs` — delete `grid_layout`/`album_play_command`/`album_card` internals; call the shared components; add pure `fmt_total_dur` + `albums_of_artist`; rebuild `album_detail` on `detail_hero`.
- `app/rune/src/views/artists.rs` — `artist_card` → `media_card` (circular); rebuild `artist_detail` (hero + Top Songs + Discography); take `cx`.
- `app/rune/src/views/genres.rs` — `genre_detail` → `detail_hero` with an icon tile + duration meta.
- `app/rune/src/main.rs` — add `State.artist_top_expanded`; init it in `State::new`; pass `cx` to `artist_detail` in `content_pane`; update/extend the render tests.
- `app/rune/src/i18n_tests.rs` — add the upgraded + new view files to the hardcoded-string scan list.
- `app/rune/locales/app.yml` — new `detail.*` and `artist.*` keys (6 locales).

---

### Task 1: `views/grid.rs` — shared responsive grid

**Files:**
- Create: `app/rune/src/views/grid.rs`
- Test: inline `#[cfg(test)] mod tests` in `app/rune/src/views/grid.rs`
- Modify: `app/rune/src/views/mod.rs`, `app/rune/src/views/albums.rs:181-186,195-196`, `app/rune/src/views/artists.rs:16,107-108`

**Interfaces:**
- Produces: `pub(crate) const MIN_CARD: f32` (= 130.0); `pub(crate) fn responsive_grid(pane_w: f32, min_card: f32, gap: f32) -> (usize, f32)` returning `(columns, card_width)`.
- Consumes: `crate::ui::space` (for `space::PANE`).

- [ ] **Step 1: Register the module.** In `app/rune/src/views/mod.rs`, add `pub mod grid;` in alphabetical position (between `genres` and `home`):

```rust
pub mod genres;
pub mod grid;
pub mod home;
```

- [ ] **Step 2: Write the failing test.** Create `app/rune/src/views/grid.rs` with only the test and a stub so it compiles-but-fails:

```rust
//! The shared responsive grid: how many equal columns of cards fill a content
//! pane, and each card's width — the original soul-player's `grid-cols-2..8`
//! breakpoints as one pure rule, reused by every browse page and the artist
//! discography. Factored out of the album grid so the pages stay consistent.
#![forbid(unsafe_code)]

use crate::ui::space;

/// Smallest card width before the grid drops a column. Tuned so the column count
/// tracks the original's `grid-cols-2..8` viewport breakpoints and cards land
/// ~144 pt at a desktop width (its `lg` tier).
pub(crate) const MIN_CARD: f32 = 130.0;

/// Columns and per-card width for a `pane_w`-wide content pane: fill the pane
/// (minus its own `space::PANE` padding on each edge), dropping a column below
/// `min_card`, with `gap` between cards. Pure, so grid and test share one rule.
pub(crate) fn responsive_grid(pane_w: f32, min_card: f32, gap: f32) -> (usize, f32) {
    let _ = (pane_w, min_card, gap, space::PANE);
    (0, 0.0) // stub — replaced in Step 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_the_pane_with_more_columns_as_it_widens() {
        // Fed the content-pane width (window minus the sidebar), the column count
        // tracks the original's grid-cols-2..8 breakpoints; cards land ~144 pt at a
        // desktop width (its `lg` tier). Values match the former albums::grid_layout.
        assert_eq!(responsive_grid(606.0, MIN_CARD, space::GUTTER).0, 4, "md-ish → 4 cols");
        let (cols, card) = responsive_grid(820.0, MIN_CARD, space::GUTTER);
        assert_eq!(cols, 5, "lg pane → 5 columns");
        assert!((card - 144.0).abs() <= 2.0, "lg cards ~144 pt, got {card}");
        assert_eq!(responsive_grid(1306.0, MIN_CARD, space::GUTTER).0, 8, "2xl → 8 cols");
        assert_eq!(responsive_grid(200.0, MIN_CARD, space::GUTTER).0, 1, "never below 1");
        assert!(responsive_grid(820.0, MIN_CARD, space::GUTTER).1 >= MIN_CARD);
    }
}
```

- [ ] **Step 3: Run the test, watch it fail.**

Run (from `app/rune/`): `cargo test grid_fills_the_pane`
Expected: FAIL — assertions fail (stub returns `(0, 0.0)`).

- [ ] **Step 4: Implement `responsive_grid`.** Replace the stub body:

```rust
pub(crate) fn responsive_grid(pane_w: f32, min_card: f32, gap: f32) -> (usize, f32) {
    let avail = (pane_w - 2.0 * space::PANE).max(min_card);
    let cols = (((avail + gap) / (min_card + gap)).floor() as usize).max(1);
    let card_w = ((avail - (cols as f32 - 1.0) * gap) / cols as f32).floor();
    (cols, card_w)
}
```

- [ ] **Step 5: Run the test, watch it pass.**

Run (from `app/rune/`): `cargo test grid_fills_the_pane`
Expected: PASS.

- [ ] **Step 6: Rewire `albums.rs` onto the shared grid.** In `app/rune/src/views/albums.rs`:
  - Delete the `grid_layout` fn (lines 177-186) **and** its old test `grid_fills_the_pane_with_more_columns_as_it_widens` (lines 485-498) — it now lives in `grid.rs`.
  - Delete the `MIN_CARD` const (lines 23-25) — now in `grid.rs`.
  - In `albums_page`, replace the card-width line:

```rust
    let pane = cx.layout_size("albums").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;
```

  - Add the import near the other `crate::views::…` uses:

```rust
use crate::views::grid;
```

- [ ] **Step 7: Rewire `artists.rs` onto the shared grid.** In `app/rune/src/views/artists.rs`:
  - Change the import on line 16 from `use crate::views::albums::{detail_track_row, grid_layout};` to:

```rust
use crate::views::albums::detail_track_row;
use crate::views::grid;
```

  - In `artists_page`, replace the card-width line:

```rust
    let pane = cx.layout_size("artists").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;
```

- [ ] **Step 8: Run the full suite + lints.**

Run (from `app/rune/`): `cargo test` then `cargo clippy --all-targets` then `cargo fmt --all`
Expected: all green; clippy clean (no unused `grid_layout`/`MIN_CARD` leftovers); fmt makes no changes.

- [ ] **Step 9: Commit.**

```bash
git add app/rune/src/views/grid.rs app/rune/src/views/mod.rs app/rune/src/views/albums.rs app/rune/src/views/artists.rs
git commit -m "refactor(views): extract shared responsive_grid helper"
```

---

### Task 2: `views/media_card.rs` — the shared library card

**Files:**
- Create: `app/rune/src/views/media_card.rs`
- Test: inline `#[cfg(test)] mod tests` in `media_card.rs` (for `card_play_command`); render coverage via existing album tests in `app/rune/src/main.rs`.
- Modify: `app/rune/src/views/mod.rs`, `app/rune/src/views/albums.rs` (replace `album_card` + `album_play_command`), `app/rune/src/main.rs` (one test-key update).

**Interfaces:**
- Produces:
  - `pub(crate) struct CardSpec { key: String, label: String, title: String, subtitle: Option<String>, cover_id: u32, circular: bool, play_ids: Vec<u32> }` (all fields `pub(crate)`).
  - `pub(crate) fn card_play_command(now_playing: Option<u32>, ids: &[u32]) -> soul_player_host::Command`.
  - `pub(crate) fn media_card(s: &State, p: &Palette, spec: CardSpec, card_w: f32, scale: f32, on_open: impl FnMut(&mut State) + 'static) -> El`.
- Consumes: `crate::views::player::cover`, `crate::views::icons`, `crate::ui::{radius, space, type_}`, `crate::State`, `crate::theme::Palette`.
- Card keys: body root = `spec.key`; the title/subtitle column = `"card-text"`; the overlay button = `"play-btn"` (so existing `click(rt, "play-btn")` tests keep working).

- [ ] **Step 1: Register the module.** In `app/rune/src/views/mod.rs` add `pub mod media_card;` alphabetically (between `library` and `player`):

```rust
pub mod library;
pub mod media_card;
pub mod player;
```

- [ ] **Step 2: Write the failing pure-fn test.** Create `app/rune/src/views/media_card.rs` with the test + a stub `card_play_command`:

```rust
//! The shared library card — the original soul-player's `MediaCard`. A cover tile
//! (square for albums, circular for artists) with a hover play-overlay, a
//! truncating title, and an optional muted subtitle. Clicking the card body runs
//! `on_open` (navigate); only the overlay button plays. Album and artist cards are
//! thin wrappers over this.
#![forbid(unsafe_code)]

use rune::{Color, Cursor, El, col, el, icon, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::views::icons;
use crate::views::player::cover;

/// The hover play-overlay button — the old app's `w-14 h-14 rounded-xl` (56 px).
const PLAY_BTN: f32 = 56.0;

/// The command a card's play-overlay issues (the old app's MediaCard
/// `handlePlayPause`): toggle pause/resume when a track of this card is the one
/// playing, else start this card's context from the top. Pure — view and test
/// share one rule. "Active" = the now-playing track belongs to this card.
pub(crate) fn card_play_command(now_playing: Option<u32>, ids: &[u32]) -> Command {
    let _ = (now_playing, ids);
    Command::TogglePlay // stub — replaced in Step 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_active_card_toggles_play_pause_otherwise_plays_from_the_start() {
        assert_eq!(
            card_play_command(Some(6), &[5, 6, 7]),
            Command::TogglePlay,
            "a track of this card is playing → toggle"
        );
        assert_eq!(
            card_play_command(Some(9), &[5, 6, 7]),
            Command::PlayContext { ids: vec![5, 6, 7], start: 0 },
            "a different context is playing → play this one from the top"
        );
        assert_eq!(
            card_play_command(None, &[5, 6, 7]),
            Command::PlayContext { ids: vec![5, 6, 7], start: 0 },
            "nothing playing → play from the top"
        );
    }
}
```

- [ ] **Step 3: Run the test, watch it fail.**

Run (from `app/rune/`): `cargo test an_active_card_toggles`
Expected: FAIL — the `Some(9)` and `None` cases return `TogglePlay` from the stub.

- [ ] **Step 4: Implement `card_play_command`.** Replace the stub body:

```rust
pub(crate) fn card_play_command(now_playing: Option<u32>, ids: &[u32]) -> Command {
    if now_playing.is_some_and(|id| ids.contains(&id)) {
        Command::TogglePlay
    } else {
        Command::PlayContext { ids: ids.to_vec(), start: 0 }
    }
}
```

- [ ] **Step 5: Run the test, watch it pass.**

Run (from `app/rune/`): `cargo test an_active_card_toggles`
Expected: PASS.

- [ ] **Step 6: Add `CardSpec` + `media_card`.** Append to `media_card.rs` (above the `#[cfg(test)]` module):

```rust
/// What a card renders: identity, artwork, and the ids its play-overlay acts on.
pub(crate) struct CardSpec {
    /// The card body's element key (e.g. `"album-0"`), unique within its grid.
    pub(crate) key: String,
    /// The accessible label (the title).
    pub(crate) label: String,
    /// The bold card title (album/artist name).
    pub(crate) title: String,
    /// An optional muted subtitle under the title (album → artist; artist → count).
    pub(crate) subtitle: Option<String>,
    /// A member track id to draw the cover from.
    pub(crate) cover_id: u32,
    /// Circular artwork (artists) vs a square `radius::MD` tile (albums).
    pub(crate) circular: bool,
    /// Every track id the play-overlay plays, in order.
    pub(crate) play_ids: Vec<u32>,
}

/// One library card: artwork (square or circular) with a hover play-overlay, a
/// title, and an optional subtitle. The card body runs `on_open`; only the overlay
/// button plays/pauses this card's context. Mirrors the original's MediaCard split
/// of button = play, body = navigate.
pub(crate) fn media_card(
    s: &State,
    p: &Palette,
    spec: CardSpec,
    card_w: f32,
    scale: f32,
    on_open: impl FnMut(&mut State) + 'static,
) -> El {
    // Circular artwork is just a cover with a half-side corner radius.
    let corner = if spec.circular { card_w * 0.5 } else { radius::MD };
    let overlay_ids = spec.play_ids.clone();
    // When this card is the active, playing context the overlay shows a pause glyph.
    let show_pause =
        s.now_playing_id.is_some_and(|id| spec.play_ids.contains(&id)) && s.playing;

    let cover_area = el()
        .key("cover-area")
        .size(card_w, card_w)
        .radius(corner)
        .shadow(2.0, 8.0, Color::BLACK.alpha(0.15))
        .child(cover(s, Some(spec.cover_id), card_w, corner, p, scale))
        .child(
            // The old app's `opacity-0 group-hover:opacity-100` overlay: ONLY this
            // button plays/pauses; the card body navigates.
            el().key("play-overlay")
                .size(card_w, card_w)
                .absolute(0.0, 0.0)
                .center()
                .group_opacity(0.0, 1.0)
                .child(
                    el().key("play-btn")
                        .role("button")
                        .label(t!("player.play").to_string())
                        .focusable(true)
                        .size(PLAY_BTN, PLAY_BTN)
                        .radius(radius::LG)
                        .fill(Color::BLACK.alpha(0.5))
                        .hover(Color::BLACK.alpha(0.3))
                        .cursor(Cursor::Pointer)
                        .center()
                        .on_press(move |st: &mut State| {
                            st.host.send(card_play_command(st.now_playing_id, &overlay_ids))
                        })
                        .child(
                            icon(
                                if show_pause { &icons::PAUSE } else { &icons::PLAY },
                                32.0,
                            )
                            .color(Color::WHITE),
                        ),
                ),
        );

    let mut text_col = col()
        .key("card-text")
        .gap(space::XXS)
        .min_w(0.0)
        .child(
            text(spec.title)
                .font(type_::CARD_TITLE)
                .color(p.fg)
                .truncate()
                .group_opacity(1.0, 0.8),
        );
    if let Some(sub) = spec.subtitle {
        text_col = text_col.child(text(sub).font(type_::ITEM).color(p.text_secondary).truncate());
    }

    col()
        .key(spec.key)
        .role("button")
        .label(spec.label)
        .focusable(true)
        .w(card_w)
        .gap(space::SM)
        .pb(space::SM)
        .cursor(Cursor::Pointer)
        .hover_group()
        .child(cover_area)
        .child(text_col)
        .on_press(on_open)
}
```

- [ ] **Step 7: Refactor `album_card` onto `media_card`.** In `app/rune/src/views/albums.rs`:
  - Delete the `PLAY_BTN` const (line 21), the `album_play_command` fn (lines 60-74), the whole `album_card` fn (lines 76-175), and the test `an_active_album_toggles_play_pause_otherwise_plays_from_the_start` (lines 457-483 — it moved to `media_card.rs`).
  - Add the import:

```rust
use crate::views::media_card::{CardSpec, media_card};
```

  - Add a new `album_card` that delegates:

```rust
/// One album card: a square cover with a hover play-overlay, the title, and the
/// artist. Clicking the body opens the album's detail page; the overlay plays.
fn album_card(s: &State, p: &Palette, slot: usize, a: &Album, card_w: f32, scale: f32) -> El {
    let name = a.name.clone();
    media_card(
        s,
        p,
        CardSpec {
            key: format!("album-{slot}"),
            label: a.name.clone(),
            title: a.name.clone(),
            subtitle: Some(a.artist.clone()),
            cover_id: a.cover_id,
            circular: false,
            play_ids: a.ids.clone(),
        },
        card_w,
        scale,
        move |st: &mut State| {
            st.route = crate::Route::Album;
            st.open_album = Some(name.clone());
            st.mobile_show_content = true;
        },
    )
}
```

  Note: this standardizes album cards to also set `mobile_show_content = true` (matching artist/genre cards — the old `album_card` omitted it, a latent mobile bug). The existing album-body-click test asserts only route + `open_album` + no commands, so it stays green.
  - Clean orphaned imports: after this refactor `albums.rs` no longer references `icon` or `Cursor` **in the card** but still uses them in `album_detail` (until Task 4) and `back_button`; leave imports that are still used. Run clippy (Step 9) to catch any that are now unused and delete exactly those.

- [ ] **Step 8: Update the one album test that keyed the old text column.** In `app/rune/src/main.rs`, test `clicking_the_album_card_body_opens_the_detail_page` (~line 6944), change the click target from the removed `"album-text"` key to the shared `"card-text"` key:

```rust
        click(&mut rt, "card-text"); // the title/artist area — not the play button
```

(Search main.rs for any other `"album-text"` occurrence and update likewise; there is one.)

- [ ] **Step 9: Run the full suite + lints.**

Run (from `app/rune/`): `cargo test` then `cargo clippy --all-targets` then `cargo fmt --all`
Expected: all green. In particular these existing main.rs tests must still pass unchanged (they exercise `media_card`): `clicking_the_card_play_button_plays_without_navigating`, `the_play_button_of_the_active_album_toggles_instead_of_restarting`, `clicking_the_album_card_body_opens_the_detail_page`. Delete any import clippy now flags as unused.

- [ ] **Step 10: Commit.**

```bash
git add app/rune/src/views/media_card.rs app/rune/src/views/mod.rs app/rune/src/views/albums.rs app/rune/src/main.rs
git commit -m "refactor(views): extract shared media_card from album card"
```

---

### Task 3: Pure helpers — `fmt_total_dur` + `albums_of_artist`

**Files:**
- Modify: `app/rune/src/views/albums.rs` (add two pure fns + their tests)
- Modify: `app/rune/locales/app.yml` (add `detail.durationHm`, `detail.durationM`)

**Interfaces:**
- Produces:
  - `pub(crate) fn fmt_total_dur(total_secs: f64) -> String` — "1 h 23 min" / "45 min", whole-minute rounding.
  - `pub(crate) fn albums_of_artist(library: &[TrackView], artist: &str) -> Vec<Album>` — the artist's tracks folded by album, first-seen order.
- Consumes: existing `group_albums`, the `Album` struct, `t!`.

- [ ] **Step 1: Add the two locale keys** to `app/rune/locales/app.yml`. Insert after the `genre.trackCount` block (the end of the existing `album/artist/genre` group, ~line 119, before the blank line and `player.nowPlaying`):

```yaml
detail.durationHm:
  en: '%{hours} h %{mins} min'
  de: '%{hours} Std. %{mins} Min.'
  es: '%{hours} h %{mins} min'
  zh: '%{hours} 小时 %{mins} 分钟'
  cs: '%{hours} h %{mins} min'
  ja: '%{hours} 時間 %{mins} 分'
detail.durationM:
  en: '%{mins} min'
  de: '%{mins} Min.'
  es: '%{mins} min'
  zh: '%{mins} 分钟'
  cs: '%{mins} min'
  ja: '%{mins} 分'
```

- [ ] **Step 2: Write the failing tests.** In `albums.rs`'s `#[cfg(test)] mod tests`, add a `TrackView` helper and two tests:

```rust
    fn atv(id: u32, artist: &str, album: &str) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
        }
    }

    #[test]
    fn total_duration_rounds_to_whole_minutes_and_formats_hours() {
        rust_i18n::set_locale("en");
        assert_eq!(fmt_total_dur(45.0 * 60.0), "45 min");
        assert_eq!(fmt_total_dur(83.0 * 60.0), "1 h 23 min");
        assert_eq!(fmt_total_dur(0.0), "0 min");
        assert_eq!(fmt_total_dur(29.0), "0 min", "under 30 s rounds down");
        assert_eq!(fmt_total_dur(31.0), "1 min", "over 30 s rounds up");
    }

    #[test]
    fn albums_of_artist_folds_only_that_artists_albums_first_seen() {
        rust_i18n::set_locale("en");
        let lib = vec![
            atv(1, "Bach", "Cello Suites"),
            atv(2, "Satie", "Gymnopedies"),
            atv(3, "Bach", "Cello Suites"),
            atv(4, "Bach", "Goldberg"),
        ];
        let albums = albums_of_artist(&lib, "Bach");
        assert_eq!(albums.len(), 2, "only Bach's two albums");
        assert_eq!(albums[0].name, "Cello Suites");
        assert_eq!(albums[0].ids, vec![1, 3], "both Cello Suites tracks fold in");
        assert_eq!(albums[1].name, "Goldberg");
        assert_eq!(albums[1].ids, vec![4]);
    }
```

- [ ] **Step 3: Run the tests, watch them fail.**

Run (from `app/rune/`): `cargo test total_duration_rounds` then `cargo test albums_of_artist_folds`
Expected: FAIL — `fmt_total_dur`/`albums_of_artist` not defined.

- [ ] **Step 4: Implement the two fns.** In `albums.rs`, after `group_albums` (~line 58), add:

```rust
/// Sum of track seconds as a coarse human total — "1 h 23 min" or "45 min"
/// (detail headers show totals, not mm:ss). Rounds to whole minutes. Pure.
pub(crate) fn fmt_total_dur(total_secs: f64) -> String {
    let mins = (total_secs.max(0.0) / 60.0).round() as u64;
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        t!("detail.durationHm", hours => h, mins => m).to_string()
    } else {
        t!("detail.durationM", mins => m).to_string()
    }
}

/// The albums of a single artist: filter the library to that artist (matching
/// `group_artists`' empty→"Unknown" bucketing), then fold by album in first-seen
/// order — the artist page's Discography. Pure, reusing the album fold.
pub(crate) fn albums_of_artist(library: &[TrackView], artist: &str) -> Vec<Album> {
    let subset: Vec<TrackView> = library
        .iter()
        .filter(|t| {
            let a = t
                .artist
                .clone()
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| t!("library.unknown").to_string());
            a == artist
        })
        .cloned()
        .collect();
    group_albums(&subset)
}
```

- [ ] **Step 5: Run the tests + i18n completeness, watch them pass.**

Run (from `app/rune/`): `cargo test total_duration_rounds albums_of_artist_folds every_key_is_translated`
Expected: PASS (all three). The completeness test proves the 2 new keys exist in all 6 locales.

- [ ] **Step 6: Commit.**

```bash
git add app/rune/src/views/albums.rs app/rune/locales/app.yml
git commit -m "feat(views): add total-duration + albums-of-artist helpers"
```

---

### Task 4: `views/detail_page.rs` + rebuild Album detail on it

**Files:**
- Create: `app/rune/src/views/detail_page.rs`
- Modify: `app/rune/src/views/mod.rs`, `app/rune/src/views/albums.rs` (rebuild `album_detail`, drop its bespoke header/pill)
- Modify: `app/rune/locales/app.yml` (add `detail.tracksAndDuration`)
- Test: album detail render tests already in `app/rune/src/main.rs` (keys `album-header`, `album-cover`, `album-play-all`, `album-track-0`) stay green; add one asserting the Play-All pill plays the album.

**Interfaces:**
- Produces:
  - `pub(crate) const DETAIL_COVER: f32` (= 176.0).
  - `pub(crate) enum HeroArt { Cover { id: u32, circular: bool }, Icon(&'static rune::Icon) }`.
  - `pub(crate) fn detail_hero(s, p, scale, prefix: &str, art: HeroArt, eyebrow: String, title: String, subtitle: Option<String>, meta: String, actions: El) -> El` — keys the row `{prefix}-header`, artwork `{prefix}-cover`, text column `{prefix}-meta`.
  - `pub(crate) fn play_all_pill(key: &str, ids: Vec<u32>, p: &Palette) -> El`.
- Consumes: `crate::views::player::cover`, `crate::views::icons`, `crate::ui::{fx, radius, space, type_}`, `soul_player_host::Command`.

- [ ] **Step 1: Register the module.** In `app/rune/src/views/mod.rs` add `pub mod detail_page;` alphabetically (after `dropdown`):

```rust
pub mod detail_page;
pub mod dropdown;
```

- [ ] **Step 2: Add the locale key.** In `app/rune/locales/app.yml`, directly after the `detail.durationM` block from Task 3, add:

```yaml
detail.tracksAndDuration:
  en: '%{count} tracks · %{dur}'
  de: '%{count} Titel · %{dur}'
  es: '%{count} pistas · %{dur}'
  zh: '%{count} 首 · %{dur}'
  cs: '%{count} skladeb · %{dur}'
  ja: '%{count} 曲 · %{dur}'
```

- [ ] **Step 3: Create `detail_page.rs`.** Write the file in full:

```rust
//! The shared detail-page hero + Play-All pill — the original soul-player's
//! `DetailPageLayout` header. Album/artist/genre detail pages all build their
//! header from `detail_hero` (artwork left, eyebrow/title/subtitle/meta/actions
//! right, reflowing to stacked on a narrow pane) and their primary action from
//! `play_all_pill`, so the three pages share one grammar.
#![forbid(unsafe_code)]

use rune::{Color, Cursor, El, Icon, col, el, icon, row, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::player::cover;

/// Large cover on a detail hero (the old app's `w-44`).
pub(crate) const DETAIL_COVER: f32 = 176.0;
/// The genre hero's icon tile (no artwork → a glyph in a muted square).
const ICON_TILE: f32 = 120.0;

/// A hero's artwork: a square album cover, a circular artist avatar, or a muted
/// icon tile (genres carry no art).
pub(crate) enum HeroArt {
    /// A cover drawn from a member track id; `circular` clips it to a circle.
    Cover { id: u32, circular: bool },
    /// A centered glyph in a muted `radius::MD` tile.
    Icon(&'static Icon),
}

fn hero_artwork(s: &State, p: &Palette, scale: f32, prefix: &str, art: HeroArt) -> El {
    match art {
        HeroArt::Cover { id, circular } => {
            let corner = if circular { DETAIL_COVER * 0.5 } else { radius::MD };
            el().key(format!("{prefix}-cover"))
                .radius(corner)
                .shadow(4.0, 16.0, Color::BLACK.alpha(0.22))
                .child(cover(s, Some(id), DETAIL_COVER, corner, p, scale))
        }
        HeroArt::Icon(glyph) => el()
            .key(format!("{prefix}-cover"))
            .size(ICON_TILE, ICON_TILE)
            .radius(radius::MD)
            .fill(p.muted)
            .center()
            .child(icon(glyph, (ICON_TILE * 0.5).round()).color(p.text_secondary)),
    }
}

/// The shared detail header: artwork left, a text block right (eyebrow, title,
/// optional subtitle, a meta line, then `actions`). Reflows to stacked when the
/// pane is too narrow (`flex_wrap`). `prefix` keys the row/cover/meta so each page
/// keeps stable keys (`album-header`, `artist-cover`, …).
#[allow(clippy::too_many_arguments)]
pub(crate) fn detail_hero(
    s: &State,
    p: &Palette,
    scale: f32,
    prefix: &str,
    art: HeroArt,
    eyebrow: String,
    title: String,
    subtitle: Option<String>,
    meta: String,
    actions: El,
) -> El {
    let mut info = col()
        .key(format!("{prefix}-meta"))
        .gap(space::XS)
        .grow(1.0)
        .min_w(0.0)
        .center_y()
        .child(text(eyebrow).font(type_::SECTION).color(p.text_secondary))
        .child(text(title).font(type_::DISPLAY).color(p.fg).truncate());
    if let Some(sub) = subtitle {
        info = info.child(
            text(sub)
                .font(type_::CARD_TITLE)
                .color(p.text_secondary)
                .truncate(),
        );
    }
    info = info
        .child(text(meta).font(type_::CAPTION).color(p.text_secondary))
        .child(actions);

    row()
        .key(format!("{prefix}-header"))
        .flex_wrap()
        .gap(space::XL)
        .pb(space::SECTION)
        .child(hero_artwork(s, p, scale, prefix, art))
        .child(info)
}

/// The "Play All" pill (the old app's `rounded-full px-6 py-3` filled Play
/// button) — plays `ids` from the top. `key` keeps each page's stable key
/// (`album-play-all`, `artist-play-all`, `genre-play-all`).
pub(crate) fn play_all_pill(key: &str, ids: Vec<u32>, p: &Palette) -> El {
    el().key(key.to_string())
        .role("button")
        .label(t!("album.playAll").to_string())
        .focusable(true)
        .px(space::XL)
        .py(space::MD)
        .radius(radius::FULL)
        .fill(p.primary)
        .cursor(Cursor::Pointer)
        .hover(p.bg.alpha(fx::HOVER_FILL))
        .press(p.bg.alpha(fx::PRESS_FILL))
        .on_press(move |st: &mut State| {
            st.host.send(Command::PlayContext { ids: ids.clone(), start: 0 })
        })
        .child(
            row()
                .gap(space::SM)
                .center_y()
                .child(icon(&icons::PLAY, 20.0).color(p.bg))
                .child(text(t!("album.playAll").to_string()).color(p.bg)),
        )
}
```

- [ ] **Step 4: Rebuild `album_detail` on the shared hero.** In `app/rune/src/views/albums.rs`:
  - Delete the old `DETAIL_COVER` const (line 218 — now in `detail_page.rs`).
  - Add imports:

```rust
use crate::views::detail_page::{HeroArt, detail_hero, play_all_pill};
```

  - Replace the whole `album_detail` fn (lines 317-451) with:

```rust
/// An album's detail page (the old app's AlbumPage): a back link, then the shared
/// hero — square cover, "ALBUM" eyebrow, title, artist, "{n} tracks · {duration}"
/// and a Play-All pill — over the album's numbered track list. The album is
/// `State::open_album`, re-derived from the current library; if it has vanished we
/// just offer the way back.
pub fn album_detail(s: &State, p: &Palette, scale: f32) -> El {
    let albums = group_albums(&s.library);
    let Some(album) = s
        .open_album
        .as_deref()
        .and_then(|name| albums.into_iter().find(|a| a.name == name))
    else {
        return col()
            .key("album-detail")
            .grow(1.0)
            .min_h(0.0)
            .padding(space::PANE)
            .child(back_button(p));
    };

    let total: f64 = album
        .ids
        .iter()
        .filter_map(|id| s.library.iter().find(|t| t.id == *id))
        .map(|t| t.duration_secs)
        .sum();
    let meta = t!(
        "detail.tracksAndDuration",
        count => album.ids.len(),
        dur => fmt_total_dur(total)
    )
    .to_string();

    let hero = detail_hero(
        s,
        p,
        scale,
        "album",
        HeroArt::Cover { id: album.cover_id, circular: false },
        t!("album.eyebrow").to_uppercase(),
        album.name.clone(),
        Some(album.artist.clone()),
        meta,
        play_all_pill("album-play-all", album.ids.clone(), p),
    );

    let mut list = col().key("album-tracks").role("list").gap(space::XXS);
    for (i, &id) in album.ids.iter().enumerate() {
        if let Some(t) = s.library.iter().find(|t| t.id == id) {
            list = list.child(detail_track_row(s.now_playing_id, s.playing, p, &album.ids, i, t));
        }
    }

    col()
        .key("album-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(
            row()
                .key("album-detail-top")
                .px(space::PANE)
                .pt(space::SM)
                .child(back_button(p)),
        )
        .child(
            col()
                .key("album-detail-content")
                .padding(space::PANE)
                .child(hero)
                .child(list)
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(24.0)
                .grow(1.0)
                .min_h(0.0),
        )
}
```

  - Now `albums.rs` no longer uses `icon`, `Color`, `type_` (for the deleted header), `fx`, `radius` in some spots, nor `icons`. **Do not guess** — run clippy (Step 7) and delete exactly the imports it flags. (The back-arrow `back_button` still uses `radius`, `fx`, `type_`, `Cursor`; `cover` is gone from albums.rs — it's used only inside `detail_page`/`media_card` now.)

- [ ] **Step 5: Add a Play-All render test.** In `app/rune/src/main.rs` tests, add a test proving the album detail's pill plays the whole album (mirrors the artist/genre ones):

```rust
    /// The album detail header's Play-All pill plays the album from the top.
    #[test]
    fn the_album_detail_play_all_plays_the_album() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![album_track(1, "T1"), album_track(2, "T2")];
        s.route = Route::Album;
        s.open_album = Some("Alb".into());
        let env = Env { system_dark: true, scale: 2.0 };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(rt.find("album-header").is_some(), "the shared hero renders");
        assert!(rt.find("album-cover").is_some(), "the hero cover renders");
        click(&mut rt, "album-play-all");
        assert_eq!(
            rt.state.host.recorded_commands(),
            vec![Command::PlayContext { ids: vec![1, 2], start: 0 }],
            "Play All plays the album from the top"
        );
    }
```

- [ ] **Step 6: Run the tests, watch them pass.**

Run (from `app/rune/`): `cargo test the_album_detail_play_all every_key_is_translated`
Expected: PASS. Also run `cargo test` to confirm the pre-existing album-detail tests (which find `album-header`/`album-track-0`) still pass.

- [ ] **Step 7: Lints.**

Run (from `app/rune/`): `cargo clippy --all-targets` then `cargo fmt --all`
Expected: clean; delete every import clippy flags as unused in `albums.rs`.

- [ ] **Step 8: Commit.**

```bash
git add app/rune/src/views/detail_page.rs app/rune/src/views/mod.rs app/rune/src/views/albums.rs app/rune/src/main.rs app/rune/locales/app.yml
git commit -m "refactor(views): rebuild album detail on shared hero + play-all pill"
```

---

### Task 5: Artist page upgrade — circular cards + Top Songs + Discography

**Files:**
- Modify: `app/rune/src/main.rs` (add `State.artist_top_expanded` field + init; pass `cx` to `artist_detail` in `content_pane`; add/adjust render tests)
- Modify: `app/rune/src/views/artists.rs` (`artist_card` → circular `media_card`; rewrite `artist_detail`)
- Modify: `app/rune/locales/app.yml` (`detail.albumsAndTracks`, `artist.topSongs`, `artist.discography`, `artist.showMore`, `artist.showLess`)

**Interfaces:**
- Consumes: `media_card`/`CardSpec` (Task 2), `detail_hero`/`HeroArt`/`play_all_pill` (Task 4), `albums_of_artist` (Task 3), `grid::responsive_grid`/`grid::MIN_CARD` (Task 1), `detail_track_row` (existing).
- Produces: `State.artist_top_expanded: bool`; `artist_detail` now takes `cx: &Cx<State>`.

- [ ] **Step 1: Add the `State` field.** In `app/rune/src/main.rs`, in the `State` struct after `open_genre: Option<String>` (line 204), add:

```rust
    /// Whether the artist detail's "Top Songs" section is expanded past the first
    /// five (toggled by its Show more/less link). Reset when a new artist opens.
    artist_top_expanded: bool,
```

- [ ] **Step 2: Init the field.** In `State::new` (line 416), after `open_genre: None,` (line 474), add:

```rust
            artist_top_expanded: false,
```

- [ ] **Step 3: Add the 5 locale keys.** In `app/rune/locales/app.yml`, after the `detail.tracksAndDuration` block (Task 4), add:

```yaml
detail.albumsAndTracks:
  en: '%{albums} albums · %{tracks} tracks'
  de: '%{albums} Alben · %{tracks} Titel'
  es: '%{albums} álbumes · %{tracks} pistas'
  zh: '%{albums} 张专辑 · %{tracks} 首'
  cs: '%{albums} alb · %{tracks} skladeb'
  ja: '%{albums} アルバム · %{tracks} 曲'
artist.topSongs:
  en: 'Top Songs'
  de: 'Top-Titel'
  es: 'Mejores canciones'
  zh: '热门歌曲'
  cs: 'Nejlepší skladby'
  ja: '人気の曲'
artist.discography:
  en: 'Discography'
  de: 'Diskografie'
  es: 'Discografía'
  zh: '唱片集'
  cs: 'Diskografie'
  ja: 'ディスコグラフィー'
artist.showMore:
  en: 'Show more'
  de: 'Mehr anzeigen'
  es: 'Mostrar más'
  zh: '显示更多'
  cs: 'Zobrazit více'
  ja: 'もっと見る'
artist.showLess:
  en: 'Show less'
  de: 'Weniger anzeigen'
  es: 'Mostrar menos'
  zh: '收起'
  cs: 'Zobrazit méně'
  ja: '折りたたむ'
```

- [ ] **Step 4: Write the failing render tests.** In `app/rune/src/main.rs` tests, add:

```rust
    /// The artist detail shows both a Top-Songs section and a Discography grid.
    #[test]
    fn the_artist_detail_has_top_songs_and_discography() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        // Two albums for Bach so the discography has cards to show.
        s.library = vec![
            artist_album_track(1, "Bach", "Cello Suites"),
            artist_album_track(2, "Bach", "Cello Suites"),
            artist_album_track(3, "Bach", "Goldberg"),
        ];
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env { system_dark: true, scale: 2.0 };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0); // second frame: discography grid reads its committed width
        assert!(rt.find("artist-header").is_some(), "the circular hero renders");
        assert!(rt.find("artist-top-songs").is_some(), "Top Songs section renders");
        assert!(rt.find("artist-discography").is_some(), "Discography section renders");
        assert!(rt.find("artist-disc-0").is_some(), "a discography album card renders");
    }

    /// A discography album card opens that album's detail page.
    #[test]
    fn a_discography_card_opens_the_album() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = vec![
            artist_album_track(1, "Bach", "Cello Suites"),
            artist_album_track(2, "Bach", "Goldberg"),
        ];
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env { system_dark: true, scale: 2.0 };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt.frame(0.0);
        click(&mut rt, "card-text"); // first disc card's title area (Cello Suites)
        assert!(matches!(rt.state.route, Route::Album), "a disc card opens the album");
        assert_eq!(rt.state.open_album.as_deref(), Some("Cello Suites"));
    }

    /// With more than five tracks, Show more expands the Top-Songs list.
    #[test]
    fn show_more_expands_top_songs() {
        let mut s = State::new(PlayerHandle::recording());
        s.sidebar_collapsed = false;
        s.library = (1..=7).map(|i| artist_album_track(i, "Bach", "Cello Suites")).collect();
        s.route = Route::Artist;
        s.open_artist = Some("Bach".into());
        let env = Env { system_dark: true, scale: 2.0 };
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        // Only the first five tracks show; the sixth row is hidden.
        assert!(rt.find("album-track-4").is_some(), "the fifth track shows");
        assert!(rt.find("album-track-5").is_none(), "the sixth is collapsed");
        assert!(rt.find("artist-top-toggle").is_some(), "a Show more toggle renders");
        click(&mut rt, "artist-top-toggle");
        assert!(rt.state.artist_top_expanded, "the toggle expands Top Songs");
        assert!(rt.find("album-track-5").is_some(), "the sixth track shows once expanded");
    }

    fn artist_album_track(id: u32, artist: &str, album: &str) -> soul_player_host::TrackView {
        soul_player_host::TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
        }
    }
```

- [ ] **Step 5: Run the tests, watch them fail.**

Run (from `app/rune/`): `cargo test the_artist_detail_has_top_songs a_discography_card_opens show_more_expands`
Expected: FAIL — `artist-top-songs`/`artist-discography`/`artist-disc-0`/`artist-top-toggle` keys don't exist yet (current artist_detail is a flat list).

- [ ] **Step 6: Refactor `artist_card` onto circular `media_card`.** In `app/rune/src/views/artists.rs`:
  - Replace the imports block (lines 10-18) so it pulls the shared components and drops the now-unused ones. The new import set:

```rust
use rune::{Cx, El, ScrollSpec, col, row, text};
use soul_player_host::{Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::{albums_of_artist, detail_track_row};
use crate::views::detail_page::{HeroArt, detail_hero, play_all_pill};
use crate::views::grid;
use crate::views::media_card::{CardSpec, media_card};
```

  (Drop `Color`, `Cursor`, `icon`, `el` from the rune import, and `crate::views::icons`, `crate::views::player::cover` — the shared components own those now. `el` is still needed for the Show-more toggle and back button — keep `el`. Final rune import: `use rune::{Cursor, Cx, El, ScrollSpec, col, el, row, text};` — reconcile against clippy in Step 10.)
  - Replace `artist_card` (lines 55-100) with:

```rust
/// One artist card: a circular avatar with a hover play-overlay, the name, and a
/// track count. Clicking the body opens the artist's detail page; the overlay
/// plays the artist's tracks. Mirrors the album card (the original's MediaCard).
fn artist_card(s: &State, p: &Palette, slot: usize, a: &Artist, card_w: f32, scale: f32) -> El {
    let name = a.name.clone();
    media_card(
        s,
        p,
        CardSpec {
            key: format!("artist-{slot}"),
            label: a.name.clone(),
            title: a.name.clone(),
            subtitle: Some(t!("artist.trackCount", count => a.ids.len()).to_string()),
            cover_id: a.cover_id,
            circular: true,
            play_ids: a.ids.clone(),
        },
        card_w,
        scale,
        move |st: &mut State| {
            st.route = crate::Route::Artist;
            st.open_artist = Some(name.clone());
            st.artist_top_expanded = false;
            st.mobile_show_content = true;
        },
    )
}
```

  - `artists_page` keeps its body but its grid-width line already changed in Task 1. Confirm it reads `grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1`.

- [ ] **Step 7: Rewrite `artist_detail`.** Replace the whole fn (lines 145-274) with the hero + Top Songs + Discography version. Note the new `cx` parameter:

```rust
/// An artist's detail page (the original's ArtistPage): a back link, a circular
/// hero ("ARTIST" eyebrow, name, "{n} albums · {n} tracks", Play All), then Top
/// Songs (first five, expandable) and a Discography grid of the artist's albums.
pub fn artist_detail(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let artists = group_artists(&s.library);
    let Some(artist) = s
        .open_artist
        .as_deref()
        .and_then(|name| artists.into_iter().find(|a| a.name == name))
    else {
        return col()
            .key("artist-detail")
            .grow(1.0)
            .min_h(0.0)
            .padding(space::PANE)
            .child(back_button(p));
    };

    let discography = albums_of_artist(&s.library, &artist.name);
    let meta = t!(
        "detail.albumsAndTracks",
        albums => discography.len(),
        tracks => artist.ids.len()
    )
    .to_string();
    let hero = detail_hero(
        s,
        p,
        scale,
        "artist",
        HeroArt::Cover { id: artist.cover_id, circular: true },
        t!("artist.eyebrow").to_uppercase(),
        artist.name.clone(),
        None,
        meta,
        play_all_pill("artist-play-all", artist.ids.clone(), p),
    );

    // Top Songs: the first five (or all, expanded), as album-art track rows.
    let shown = if s.artist_top_expanded {
        artist.ids.len()
    } else {
        artist.ids.len().min(5)
    };
    let mut top = col()
        .key("artist-top-songs")
        .gap(space::SM)
        .pb(space::SECTION)
        .child(text(t!("artist.topSongs").to_string()).font(type_::HEADING).color(p.fg));
    for (i, &id) in artist.ids.iter().take(shown).enumerate() {
        if let Some(t) = s.library.iter().find(|t| t.id == id) {
            top = top.child(detail_track_row(s.now_playing_id, s.playing, p, &artist.ids, i, t));
        }
    }
    if artist.ids.len() > 5 {
        let label = if s.artist_top_expanded {
            t!("artist.showLess")
        } else {
            t!("artist.showMore")
        };
        top = top.child(
            el().key("artist-top-toggle")
                .role("button")
                .label(label.to_string())
                .focusable(true)
                .px(space::SM)
                .py(space::XS)
                .radius(radius::SM)
                .cursor(Cursor::Pointer)
                .hover(p.fg.alpha(fx::HOVER))
                .press(p.fg.alpha(fx::PRESS))
                .child(text(label.to_string()).font(type_::ITEM).color(p.accent))
                .on_press(|st: &mut State| st.artist_top_expanded = !st.artist_top_expanded),
        );
    }

    // Discography: the artist's albums as a responsive card grid; a card opens the
    // album. Read this section's own committed width for the column count.
    let pane = cx.layout_size("artist-discography").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;
    let mut disc = col()
        .key("artist-discography")
        .gap(space::SM)
        .child(text(t!("artist.discography").to_string()).font(type_::HEADING).color(p.fg));
    if !discography.is_empty() {
        let mut grid_el = row()
            .key("artist-disc-grid")
            .role("grid")
            .flex_wrap()
            .gap(space::GUTTER);
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
                    st.route = crate::Route::Album;
                    st.open_album = Some(name.clone());
                    st.mobile_show_content = true;
                },
            ));
        }
        disc = disc.child(grid_el);
    }

    col()
        .key("artist-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(
            row()
                .key("artist-detail-top")
                .px(space::PANE)
                .pt(space::SM)
                .child(back_button(p)),
        )
        .child(
            col()
                .key("artist-detail-content")
                .padding(space::PANE)
                .child(hero)
                .child(top)
                .child(disc)
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(24.0)
                .grow(1.0)
                .min_h(0.0),
        )
}
```

- [ ] **Step 8: Thread `cx` through the dispatch.** In `app/rune/src/main.rs` `content_pane` (line 1754), change:

```rust
            Route::Artist => views::artists::artist_detail(s, p, cx, scale).grow(1.0),
```

- [ ] **Step 9: Update the pre-existing artist tests for the new structure.** In `app/rune/src/main.rs`:
  - In `clicking_an_artist_card_opens_the_detail_page` (~line 6540), change the click target from `"artist-0"` to `"card-text"` (the card now has a hover play-overlay over the avatar, so target the title area to open the detail — same as the album card):

```rust
        click(&mut rt, "card-text"); // the first grouped artist's title (Bach)
```

  - `the_artist_detail_lists_tracks_with_play_all_and_back` (~line 6570) still asserts `artist-header`, `artist-back`, `album-track-0`, and the `artist-play-all` command — all still present, so it stays as-is. (Optionally tighten its comment; no code change required.)

- [ ] **Step 10: Run the tests + lints, watch them pass.**

Run (from `app/rune/`): `cargo test` then `cargo clippy --all-targets` then `cargo fmt --all`
Expected: the three new artist tests pass; the two updated/kept artist tests pass; every other test green; clippy clean (reconcile the `artists.rs` rune import list against what's actually used — e.g. keep `Cursor`/`el`, drop `Color`/`icon`); fmt makes no changes. Also confirm `cargo test every_key_is_translated` is green (5 new keys × 6 locales).

- [ ] **Step 11: Commit.**

```bash
git add app/rune/src/views/artists.rs app/rune/src/main.rs app/rune/locales/app.yml
git commit -m "feat(views): artist page with circular avatar, top songs, discography"
```

---

### Task 6: Genre detail — shared hero with an icon tile

**Files:**
- Modify: `app/rune/src/views/genres.rs` (`genre_detail` → `detail_hero` icon tile + duration meta)
- Test: genre detail render tests in `app/rune/src/main.rs` (`genre-header`, `genre-play-all`) stay green; the existing `the_genre_detail_lists_tracks_with_play_all_and_back` covers it.

**Interfaces:**
- Consumes: `detail_hero`/`HeroArt::Icon`/`play_all_pill` (Task 4), `fmt_total_dur` (Task 3), `detail.tracksAndDuration` key (Task 4), `icons::MUSIC` (existing), `detail_track_row` (existing).
- Genre browse list (`group_genres`, `genre_row`, `genres_page`) is unchanged — genres have no art, so the list stays.

- [ ] **Step 1: Update imports.** In `app/rune/src/views/genres.rs`, replace the imports (lines 12-19) with:

```rust
use rune::{Cursor, El, ScrollSpec, col, el, text};
use soul_player_host::{Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::{detail_track_row, fmt_total_dur};
use crate::views::detail_page::{HeroArt, detail_hero, play_all_pill};
use crate::views::icons;
```

  (`row`/`icon` are only needed by the old bespoke header + pill, which `detail_hero`/`play_all_pill` replace; keep `icon`-bearing `icons` import because `HeroArt::Icon(&icons::MUSIC)` references the static. Reconcile against clippy in Step 5.)

- [ ] **Step 2: Rebuild `genre_detail` on the shared hero.** Replace the fn (lines 132-235) with:

```rust
/// A genre's detail page: a back link, the shared hero using a muted music-note
/// tile in place of artwork ("GENRE" eyebrow, name, "{n} tracks · {duration}",
/// Play All), then the genre's track list. The genre is `State::open_genre`,
/// re-derived from the current library; if it has vanished we just offer the way
/// back.
pub fn genre_detail(s: &State, p: &Palette, scale: f32) -> El {
    let genres = group_genres(&s.library);
    let Some(genre) = s
        .open_genre
        .as_deref()
        .and_then(|name| genres.into_iter().find(|g| g.name == name))
    else {
        return col()
            .key("genre-detail")
            .grow(1.0)
            .min_h(0.0)
            .padding(space::PANE)
            .child(back_button(p));
    };

    let total: f64 = genre
        .ids
        .iter()
        .filter_map(|id| s.library.iter().find(|t| t.id == *id))
        .map(|t| t.duration_secs)
        .sum();
    let meta = t!(
        "detail.tracksAndDuration",
        count => genre.ids.len(),
        dur => fmt_total_dur(total)
    )
    .to_string();

    let hero = detail_hero(
        s,
        p,
        scale,
        "genre",
        HeroArt::Icon(&icons::MUSIC),
        t!("genre.eyebrow").to_uppercase(),
        genre.name.clone(),
        None,
        meta,
        play_all_pill("genre-play-all", genre.ids.clone(), p),
    );

    let mut list = col().key("genre-tracks").role("list").gap(space::XXS);
    for (i, &id) in genre.ids.iter().enumerate() {
        if let Some(t) = s.library.iter().find(|t| t.id == id) {
            list = list.child(detail_track_row(s.now_playing_id, s.playing, p, &genre.ids, i, t));
        }
    }

    col()
        .key("genre-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(
            row_top(p),
        )
        .child(
            col()
                .key("genre-detail-content")
                .padding(space::PANE)
                .child(hero)
                .child(list)
                .scroll(ScrollSpec { stick_bottom: false })
                .scroll_edge_fade(24.0)
                .grow(1.0)
                .min_h(0.0),
        )
}

/// The back-link bar above the genre detail content.
fn row_top(p: &Palette) -> El {
    use rune::row;
    row()
        .key("genre-detail-top")
        .px(space::PANE)
        .pt(space::SM)
        .child(back_button(p))
}
```

  Note: `genre_detail` gains a `scale: f32` parameter (the hero draws no cover but `detail_hero` requires it). The `row_top` helper keeps `row` local so the top-of-file import list stays minimal; if you prefer, instead add `row` back to the rune import and inline it — either is fine, pick one and keep clippy clean.

- [ ] **Step 3: Update the dispatch for the new `scale` arg.** In `app/rune/src/main.rs` `content_pane` (line 1756), change:

```rust
            Route::Genre => views::genres::genre_detail(s, p, scale).grow(1.0),
```

- [ ] **Step 4: Run the genre tests, watch them pass.**

Run (from `app/rune/`): `cargo test genre`
Expected: PASS — `the_genre_detail_lists_tracks_with_play_all_and_back` (finds `genre-header`, `genre-back`, `album-track-0`, clicks `genre-play-all`) and `clicking_a_genre_opens_the_detail_page` both green. The shared hero keys the row `genre-header` and the page passes the pill keyed `genre-play-all`, so the existing assertions hold.

- [ ] **Step 5: Lints.**

Run (from `app/rune/`): `cargo clippy --all-targets` then `cargo fmt --all`
Expected: clean; delete any genre.rs import clippy flags unused.

- [ ] **Step 6: Commit.**

```bash
git add app/rune/src/views/genres.rs app/rune/src/main.rs
git commit -m "refactor(views): genre detail on shared hero with icon tile"
```

---

### Task 7: i18n scan-list — cover the upgraded + new view files

**Files:**
- Modify: `app/rune/src/i18n_tests.rs:87-93` (the `views_have_no_hardcoded_ui_strings` source list)

**Interfaces:**
- Consumes: the finished `albums.rs`/`artists.rs`/`genres.rs`/`media_card.rs`/`detail_page.rs`/`grid.rs`/`home.rs` — all of which must already route every prose literal through `t!` (verified: the only bare literals are the `"←"` back glyphs, which the scanner ignores).

- [ ] **Step 1: Write the failing expansion.** In `app/rune/src/i18n_tests.rs`, replace the `sources` array (lines 87-93) with the expanded list:

```rust
    let sources = [
        ("sidebar.rs", include_str!("views/sidebar.rs")),
        ("player.rs", include_str!("views/player.rs")),
        ("library.rs", include_str!("views/library.rs")),
        ("settings.rs", include_str!("views/settings.rs")),
        ("queue.rs", include_str!("views/queue.rs")),
        ("home.rs", include_str!("views/home.rs")),
        ("albums.rs", include_str!("views/albums.rs")),
        ("artists.rs", include_str!("views/artists.rs")),
        ("genres.rs", include_str!("views/genres.rs")),
        ("media_card.rs", include_str!("views/media_card.rs")),
        ("detail_page.rs", include_str!("views/detail_page.rs")),
        ("grid.rs", include_str!("views/grid.rs")),
    ];
```

- [ ] **Step 2: Run the scan test.**

Run (from `app/rune/`): `cargo test views_have_no_hardcoded_ui_strings`
Expected: PASS. If it FAILS, the failure message lists `file:line` offenders — wrap each flagged literal in `t!("…")` (adding the key to all 6 locales in `app.yml`) and re-run. (Pre-vetted: the added files contain no prose literals, only the `"←"` glyph, so this should pass first try. Do NOT disable or weaken the scanner.)

- [ ] **Step 3: Full suite + lints, final green gate.**

Run (from `app/rune/`): `cargo test` then `cargo clippy --all-targets` then `cargo fmt --all --check`
Expected: entire `app/rune` suite green; clippy clean; fmt reports no diff.

- [ ] **Step 4: Commit.**

```bash
git add app/rune/src/i18n_tests.rs
git commit -m "test(i18n): scan content pages for hardcoded UI strings"
```

---

## Post-Implementation: Visual Verification (not a code task, but do it)

The render tests assert structure (keys, routes, commands) but not pixels. Before opening the PR, sanity-check the look against the original with the headless snapshotter (per the rune-poll-driven-animation / dropdown memory notes, GPU views verify via `--snap`):

- Album detail: `cargo run -- --snap --scene album --at 0.0` (or the project's established `--snap` invocation) — confirm square cover + "ALBUM" eyebrow + "{n} tracks · {dur}" + Play-All pill + numbered rows.
- Artist detail: confirm the circular avatar, "{n} albums · {n} tracks", Top Songs (5 + Show more), and the Discography grid reflow.
- Genre detail: confirm the muted music-note tile, "GENRE" eyebrow, "{n} tracks · {dur}".
- Browse grids: resize and confirm 2→8 column reflow still matches `grid-cols-2..8`.

Document any blur/circular-clip limitation found (the spec flagged the blurred-avatar fallback as best-effort) in the PR description.

## Final: Push & PR

```bash
git push -u origin feat/content-pages --no-verify   # only if a hook misfires
gh pr create --fill --base main
```

Per the workspace notes: `app/rune` CI runs on ubuntu/macOS only (the headless Windows runner crashes on click-focus). After CI is green, merge with `gh pr merge --squash`.

---

## Self-Review (completed by plan author)

**Spec coverage:**
- Shared `media_card` (square + circular, hover play-overlay) → Task 2 + Task 5 (circular). ✅
- Shared `detail_hero` + `play_all_pill` → Task 4. ✅
- Shared `responsive_grid` factored from `grid_layout` → Task 1. ✅
- Reuse `detail_track_row`/`cover`/`fmt_dur` → kept as-is; new `fmt_total_dur` for coarse totals → Task 3. ✅
- Album detail: square cover, ALBUM eyebrow, "{n} tracks · {duration}", Play All, numbered rows → Task 4. ✅
- Artist (main upgrade): circular avatar hero, "{n} albums · {n} tracks", Play All, Top Songs (5 + Show more via `artist_top_expanded`), Discography (`albums_of_artist` → album-card grid → opens Album) → Tasks 3 + 5. ✅
- Genre: keep name list browse; detail = shared hero + icon tile + "{n} tracks · {duration}" → Task 6. ✅
- Data app-side, no backend; `State.artist_top_expanded` added; name-key routes kept → Tasks 3/5. ✅
- Tests: pure-fn (`responsive_grid`, `card_play_command`, `fmt_total_dur`, `albums_of_artist`) + render (top-songs/discography/show-more/disc-card-nav/play-all) + i18n 6 locales + scan-list expansion → every task. ✅
- Deferred (year, playlists, disc list-toggle, genre search, back-stack, blurred avatar) → untouched, documented. ✅

**Type consistency:** `responsive_grid(pane_w, min_card, gap) -> (usize, f32)` used identically in albums/artists; `CardSpec` field names match across `media_card`/`album_card`/`artist_card`/discography; `detail_hero(..., prefix, art, eyebrow, title, subtitle, meta, actions)` and `play_all_pill(key, ids, p)` used identically in album/artist/genre; `HeroArt::{Cover{id,circular}, Icon(&'static Icon)}` consistent; `artist_detail` gains `cx`, `genre_detail` gains `scale`, both reflected in the `content_pane` dispatch. ✅

**Placeholders:** none — every code step shows complete code; stubs are explicitly temporary (Steps that introduce them are immediately followed by a fail-run then the real implementation). ✅
