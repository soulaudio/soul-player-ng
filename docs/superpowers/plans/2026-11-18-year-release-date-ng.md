# Year / Release-Date Metadata — Phase 2 (soul-player-ng) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface the album year the core (Phase 1, `.soulidx` v5) now bakes — "Artist • 1973" on cards and detail headers, a "reissued" line, a Settings **Original | Release** toggle that re-labels everywhere and re-sorts the Artist Discography client-side.

**Architecture:** The backend already holds the catalog in `views_by`; add the two packed dates to `TrackView`, carry them onto the grouped `Album`, and render/sort by a user-selected date. No further core change — Phase 1 (core `ddc4817`) is already pinned across the three lockfiles (commit `60a0ac5`).

**Tech Stack:** Rust; `soul-player-backend` (projection), `app/rune` (Rune UI, nightly toolchain), `config.toml` settings, `rust-i18n` (6 locales).

## Global Constraints

- The core is already bumped to `ddc4817` across all three `-ng` lockfiles (done). Do NOT touch lockfiles or re-bump core.
- A **packed date** is a `u32` = `(year<<9)|(month<<5)|day`; `0` = unknown. Unpack: `year = (d >> 9) as u16`; known = `d != 0`; chronological sort key = `if d == 0 { u32::MAX } else { d }` (unknown sorts LAST). `-ng` replicates this unpack locally (2 lines) — no need to add a `soul-audio-catalog` dep.
- Selected date = `Original → original_date` else `release_date`; when the selected one is unknown (`0`), fall back to the OTHER; when both are `0`, the year is unknown (show artist alone / "—").
- Default setting = **Original**. The toggle affects: the displayed year EVERYWHERE, and the sort order of the **Artist Discography only** (the Albums grid keeps its baked artist-grouped order, re-labeled).
- New user-facing strings get `t!` keys in ALL 6 locales (`app/rune/locales/app.yml`): en, de, es, zh, cs + the 6th present there.
- Run `app/rune` cargo from `app/rune/` (nightly-2026-11-10, auto-selected from CWD). `soul-player-backend` runs from the repo root.
- Commit messages: NO Claude/Anthropic/AI/Co-Authored-By attribution (the commit-msg hook rejects them).
- No em-dashes in UI copy or commit messages (user preference) — use commas/colons/parens.
- Verify per workspace: backend `cargo test -p soul-player-backend`; app/rune `cargo test` (from `app/rune/`) + `cargo clippy --all-targets -- -D warnings` + `cargo fmt --all -- --check` (the `--all-targets` clippy lints the test module, which `--bin` skips; keep lines off the 100-col boundary so local and CI rustfmt agree).

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/soul-player-backend/src/types.rs` | `TrackView` DTO | +`original_date: u32`, +`release_date: u32` (T1) |
| `crates/soul-player-backend/src/library.rs` | catalog → `TrackView` projection | `album_dates` resolver + populate in `views_by` (T1) |
| `app/rune/src/config.rs` | `config.toml` model | `AlbumYearSource` enum + Config field + load/save pair (T2) |
| `app/rune/src/main.rs` | `State` + setters + `TrackView` test sites | `album_year` field + `set_album_year` + the new fields on all test `TrackView{}` (T1/T2) |
| `app/rune/src/views/settings.rs` | Settings UI | an Original/Release segmented control in the Appearance card (T2) |
| `app/rune/src/views/albums.rs` | `Album` group + cards + discography | `Album` carries dates; `selected_year` helper; "Artist • year" subtitle; discography re-sort (T3/T4) |
| `app/rune/src/views/detail_page.rs` | shared detail hero | a "reissued" line (T3) |
| `app/rune/locales/app.yml` | i18n | `settings.albumYear*`, `detail.reissued` in 6 locales (T2/T3) |

---

## Task 1: `TrackView` carries the album's packed dates (backend)

**Files:**
- Modify: `crates/soul-player-backend/src/types.rs:226` (`TrackView`), `crates/soul-player-backend/src/library.rs:291-311` (`views_by`) + a new resolver near `album_title` (~`:361`)
- Modify (compile ripple): every `TrackView { … }` literal in `app/rune/src/{main.rs,views/albums.rs,views/artists.rs,views/genres.rs}` (~26 test/synthesis sites) — set the two new fields to `0`.
- Test: `crates/soul-player-backend/src/library.rs` test module (or an existing projection test).

**Interfaces:**
- Produces: `TrackView { …, pub original_date: u32, pub release_date: u32 }` (packed; `0` = unknown). `fn album_dates(cat: &MultiCatalog, album: u32) -> (u32, u32)`.

- [ ] **Step 1: Write the failing test**

In `crates/soul-player-backend/src/library.rs` tests, scan a tiny fixture library whose album is tagged with an original + release date and assert the projected `TrackView` carries them. If the existing test harness bakes an index from tagged files, extend it; otherwise assert on a `views_by(BrowseAxis::ArtistAlbumTrack)` result that the album's track has `original_date != 0` matching the baked `AlbumRec`. Model it on the nearest existing `views_by`/projection test in the file.

```rust
#[test]
fn track_view_carries_album_original_and_release_dates() {
    // Build/scan a library with an album dated original=1973, release=2015
    // (reuse the file's existing index-baking test helper). Then:
    let views = lib.views_by(BrowseAxis::ArtistAlbumTrack);
    let t = views.iter().find(|t| t.album.as_deref() == Some("Test Album")).unwrap();
    // 1973 packed as a year-only date = 1973 << 9.
    assert_eq!(t.original_date, (1973u32) << 9);
    assert_eq!(t.release_date, (2015u32) << 9);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p soul-player-backend track_view_carries_album`
Expected: FAIL — `TrackView` has no field `original_date`.

- [ ] **Step 3: Add the fields + resolver + projection**

`types.rs` — after `pub remote: bool,` (`:242`) inside `TrackView`:
```rust
    /// The album's ORIGINAL-release date, packed `(year<<9)|(month<<5)|day`
    /// (`0` = unknown) — mirrors `soul_audio_catalog::PackedDate`. The audiophile
    /// canonical year; the UI prefers it, falling back to `release_date`.
    pub original_date: u32,
    /// The album's main release/reissue date, same packing. Unknown = `0`.
    pub release_date: u32,
```

`library.rs` — add beside `album_title` (`:361`):
```rust
/// The album's two packed dates (`soul_audio_catalog::PackedDate` as raw `u32`):
/// `(original_date, release_date)`, `0` when the album id is out of range.
fn album_dates(cat: &MultiCatalog, album: u32) -> (u32, u32) {
    cat.album(album)
        .map(|a| (a.original_date.0, a.release_date.0))
        .unwrap_or((0, 0))
}
```

`library.rs` `views_by` — in the `TrackView { … }` literal (`:297-309`), after `remote: false,` add:
```rust
                    original_date: album_dates(&cat, t.album).0,
                    release_date: album_dates(&cat, t.album).1,
```
(Or bind `let (od, rd) = album_dates(&cat, t.album);` before the struct and use `od`/`rd` — preferred, one lookup.)

- [ ] **Step 4: Fix the compile ripple**

Every other `TrackView { … }` literal (the ~26 synthesis/test sites in `app/rune/src/main.rs`, `views/albums.rs`, `views/artists.rs`, `views/genres.rs`) now misses two fields. Add `original_date: 0, release_date: 0,` to each (they are non-date test fixtures). A test helper that builds `TrackView` (e.g. an `atv(...)`/`tv(...)` fn) is the DRY fix — update the helper once if one exists; otherwise update each literal.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p soul-player-backend` then (from `app/rune/`) `cargo test`
Expected: PASS — the projection test passes; all app/rune tests still compile and pass with the two added fields.

- [ ] **Step 6: Commit**

```bash
git add crates/soul-player-backend/src/ app/rune/src/
git commit -m "feat(backend): project album original/release dates onto TrackView"
```

---

## Task 2: Settings "Album year: Original | Release" (config + state + UI)

**Files:**
- Modify: `app/rune/src/config.rs` (`Config` struct `:84`; a load/save pair modeled on `load_theme_pref`/`save_theme_pref` `:441-446`)
- Modify: `app/rune/src/main.rs` (`State` near `theme_pref` `:293`; a setter modeled on `set_theme_pref` `:837`; load at startup near the theme-pref load)
- Modify: `app/rune/src/views/settings.rs` (a segmented control in the Appearance card, modeled on `theme_button` `:1088`)
- Modify: `app/rune/locales/app.yml` (6 locales)
- Test: `app/rune/src/config.rs` tests + a render test in `main.rs`.

**Interfaces:**
- Produces: `pub enum AlbumYearSource { Original, Release }` (`Default = Original`, `Clone, Copy, PartialEq`, serde `rename_all = "lowercase"`); `config::load_album_year() -> AlbumYearSource`; `config::save_album_year(AlbumYearSource)`; `State.album_year: AlbumYearSource`; `set_album_year(&mut State, AlbumYearSource)`.

- [ ] **Step 1: Write the failing config round-trip test**

In `app/rune/src/config.rs` tests (use the existing `use_test_config_dir()` isolation, as the theme-pref tests do):
```rust
#[test]
fn album_year_source_round_trips_and_defaults_to_original() {
    let _g = use_test_config_dir();
    assert_eq!(load_album_year(), AlbumYearSource::Original); // default
    save_album_year(AlbumYearSource::Release);
    assert_eq!(load_album_year(), AlbumYearSource::Release);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (from `app/rune/`): `cargo test album_year_source_round_trips`
Expected: FAIL — `AlbumYearSource`/`load_album_year` undefined.

- [ ] **Step 3: Implement the config layer**

`config.rs` — add the enum (near `ThemePref`), a `#[serde(default)]` field `album_year_source: AlbumYearSource` on `Config` (`:84`, so old files load), a `Default` entry, and the pair:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlbumYearSource {
    #[default]
    Original,
    Release,
}

/// The saved album-year source (which date the UI shows/sorts by). Default Original.
pub fn load_album_year() -> AlbumYearSource {
    load().album_year_source
}
pub fn save_album_year(src: AlbumYearSource) {
    let mut cfg = load();
    cfg.album_year_source = src;
    save(&cfg);
}
```

- [ ] **Step 4: Wire State + setter (main.rs)**

- Add `album_year: AlbumYearSource` to `State` (near `theme_pref` `:293`), load it in `State::new`/startup where `theme_pref` is loaded.
- Add the setter (near `set_theme_pref` `:837`):
```rust
pub(crate) fn set_album_year(s: &mut State, src: config::AlbumYearSource) {
    s.album_year = src;
    config::save_album_year(src);
}
```

- [ ] **Step 5: Add the Settings control (settings.rs) + i18n**

In the Appearance card, add a two-button segmented control (modeled on `theme_button` `:1088`): buttons "Original" / "Release", the active one filled `p.primary`, each `on_press` calling `set_album_year(st, AlbumYearSource::{Original|Release})`. Label the row `t!("settings.albumYear")`. Add keys to all 6 locales in `app.yml`:
```yaml
settings:
  albumYear: 'Album year'
  albumYearOriginal: 'Original'
  albumYearRelease: 'Release'
```
(translate per locale; keep existing single-quote YAML style.)

- [ ] **Step 6: Render test**

In `main.rs` tests, render Settings → Appearance and assert both album-year buttons exist (keys e.g. `album-year-original`/`album-year-release`) and pressing `album-year-release` flips `State.album_year`. Model it on the existing theme-button render test.

- [ ] **Step 7: Run tests + commit**

Run (from `app/rune/`): `cargo test album_year` then the targeted render test.
```bash
git add app/rune/src/config.rs app/rune/src/main.rs app/rune/src/views/settings.rs app/rune/locales/app.yml
git commit -m "feat(settings): Album year Original/Release toggle (config + state + UI)"
```

---

## Task 3: Display the selected year ("Artist • 1973" + "reissued")

**Files:**
- Modify: `app/rune/src/views/albums.rs` (`Album` struct `:24`; `group_albums` `:41`; a `selected_year` helper; `album_card` subtitle `:97`; `album_detail` subtitle/meta)
- Modify: `app/rune/src/views/detail_page.rs` (a "reissued" line, or compose it into the `album_detail` meta string)
- Modify: `app/rune/locales/app.yml` (`detail.reissued`, 6 locales)
- Test: `albums.rs` test module.

**Interfaces:**
- Consumes: `TrackView.original_date`/`release_date` (T1); `State.album_year`/`AlbumYearSource` (T2).
- Produces: `Album { …, original_date: u32, release_date: u32 }`; `fn album_year(a: &Album, src: AlbumYearSource) -> Option<u16>` (selected-with-fallback → year, `None` if both unknown).

- [ ] **Step 1: Write the failing helper test**

```rust
#[test]
fn album_year_prefers_selected_source_with_fallback() {
    use crate::config::AlbumYearSource::*;
    let a = Album { name: "x".into(), artist: "y".into(), cover_id: 0, ids: vec![],
                    original_date: 1973u32 << 9, release_date: 2015u32 << 9 };
    assert_eq!(album_year(&a, Original), Some(1973));
    assert_eq!(album_year(&a, Release), Some(2015));
    // fallback: release known, original unknown, Original selected → release
    let b = Album { original_date: 0, release_date: 2015u32 << 9, ..a.clone() };
    assert_eq!(album_year(&b, Original), Some(2015));
    // both unknown → None
    let c = Album { original_date: 0, release_date: 0, ..a.clone() };
    assert_eq!(album_year(&c, Original), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (from `app/rune/`): `cargo test album_year_prefers_selected`
Expected: FAIL — `Album` has no `original_date`; `album_year` undefined.

- [ ] **Step 3: Implement**

`Album` struct (`:24`) — add `pub(crate) original_date: u32,` and `pub(crate) release_date: u32,` (derive `Clone` on `Album` if the test uses `..a.clone()`; add `#[derive(Clone)]`).
`group_albums` (`:41`) — when first pushing an album, set the two dates from the first track (first-wins, like `cover_id`):
```rust
            albums.push(Album {
                name,
                artist: t.artist.clone().unwrap_or_default(),
                cover_id: t.id,
                ids: Vec::new(),
                original_date: t.original_date,
                release_date: t.release_date,
            });
```
Add the helper:
```rust
/// The album's display year for the chosen source: the selected date, falling
/// back to the other when the selected one is unknown; `None` if both are unknown.
/// Packed `u32` → year via `>> 9`; `0` = unknown.
pub(crate) fn album_year(a: &Album, src: crate::config::AlbumYearSource) -> Option<u16> {
    use crate::config::AlbumYearSource::Original;
    let (first, second) = if src == Original {
        (a.original_date, a.release_date)
    } else {
        (a.release_date, a.original_date)
    };
    let d = if first != 0 { first } else { second };
    (d != 0).then(|| (d >> 9) as u16)
}
```

- [ ] **Step 4: Render the year on the card + detail**

- `album_card` (`:97`): compute `let sub = match album_year(a, s.album_year) { Some(y) => format!("{} · {y}", a.artist), None => a.artist.clone() };` (use a middot, not an em-dash) and set `subtitle: Some(sub)`. When the artist is empty, show just the year.
- `album_detail` hero: same selected-year into the subtitle or the meta line; when `release_date != original_date` and both are known, append a `t!("detail.reissued", year => release_year)` note to the hero meta (via `detail_page.rs` — compose it into the `meta` string passed to `detail_hero`, the simplest seam).
- Add `detail.reissued: 'reissued {year}'` to all 6 locales.

- [ ] **Step 5: Render test for the card subtitle**

A render test: a seeded 1-album library (original 1973) on the Albums page shows a card subtitle containing "1973"; an untagged album shows the artist alone (no stray "· 0").

- [ ] **Step 6: Run tests + commit**

Run (from `app/rune/`): `cargo test album_year` + the render test.
```bash
git add app/rune/src/views/albums.rs app/rune/src/views/detail_page.rs app/rune/locales/app.yml
git commit -m "feat(views): show the selected album year on cards and detail, with a reissued note"
```

---

## Task 4: Re-sort the Discography on the toggle

**Files:**
- Modify: `app/rune/src/views/albums.rs` (`albums_of_artist` `:70`)
- Test: `albums.rs` test module.

**Interfaces:**
- Consumes: `album_year`/the packed dates (T3), `State.album_year` (T2).

- [ ] **Step 1: Write the failing re-sort test**

```rust
#[test]
fn discography_sorts_by_the_selected_date() {
    use crate::config::AlbumYearSource::*;
    // Two albums by one artist: A original=1990/release=2010, B original=1980/release=2020.
    let lib = /* two albums, first-seen order A then B */;
    let by_original = albums_of_artist(&lib, "Artist", Original);
    assert_eq!(by_original.iter().map(|a| &a.name).collect::<Vec<_>>(), vec!["B", "A"]); // 1980 < 1990
    let by_release = albums_of_artist(&lib, "Artist", Release);
    assert_eq!(by_release.iter().map(|a| &a.name).collect::<Vec<_>>(), vec!["A", "B"]); // 2010 < 2020
}
```

- [ ] **Step 2: Run test to verify it fails**

Run (from `app/rune/`): `cargo test discography_sorts_by_the_selected`
Expected: FAIL — `albums_of_artist` takes 2 args, not 3.

- [ ] **Step 3: Implement**

Change `albums_of_artist` to take the source and sort the grouped albums by the selected date's sort key (unknown last), stable within equal years:
```rust
pub(crate) fn albums_of_artist(
    library: &[TrackView],
    artist: &str,
    src: crate::config::AlbumYearSource,
) -> Vec<Album> {
    let subset: Vec<TrackView> = library.iter().filter(|t| {
        let a = t.artist.clone().filter(|a| !a.is_empty())
            .unwrap_or_else(|| t!("library.unknown").to_string());
        a == artist
    }).cloned().collect();
    let mut albums = group_albums(&subset);
    // Chronological by the SELECTED date; unknown (0) sorts last. Stable keeps
    // the first-seen order among equal/unknown years.
    let key = |a: &Album| -> u32 {
        let (first, second) = if src == crate::config::AlbumYearSource::Original {
            (a.original_date, a.release_date)
        } else {
            (a.release_date, a.original_date)
        };
        let d = if first != 0 { first } else { second };
        if d == 0 { u32::MAX } else { d }
    };
    albums.sort_by_key(key);
    albums
}
```
Update the one caller in `artists.rs` (`artist_detail`'s `albums_of_artist(&s.library, …)`) to pass `s.album_year`.

- [ ] **Step 4: Run tests + commit**

Run (from `app/rune/`): `cargo test discography_sorts` + the full app/rune suite.
```bash
git add app/rune/src/views/albums.rs app/rune/src/views/artists.rs
git commit -m "feat(views): re-sort the Artist discography by the selected album year"
```

---

## After all tasks

- From `app/rune/`: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. From the root: `cargo test -p soul-player-backend`.
- Then superpowers:finishing-a-development-branch: push `feat/year-metadata`, open a PR against `-ng` main, wait for CI (`rust (service+host)` + `rune` legs) green, merge `gh pr merge --squash --delete-branch`. The branch already carries the spec (`bbdb723`) and the core bump (`60a0ac5`).

---

## Self-Review

**Spec coverage:** TrackView dual dates + projection (T1 ✓); Settings Original|Release toggle persisted (T2 ✓); "Artist • year" + reissued display (T3 ✓); discography client re-sort on toggle, Albums grid baked order kept (T4 ✓); i18n 6 locales (T2/T3 ✓); core bump + root-lock reconcile (done pre-plan, commit 60a0ac5). Deferred per spec: `{OriginalYear}` token, per-track dates, sorting the global Albums grid.

**Placeholder scan:** T1 Step 1 and T4 Step 1 reference "reuse the file's existing index-baking test helper" / "two albums first-seen order" rather than inlining a full fixture — the assertion and packed-date math are fully specified; the fixture builder is an existing in-file pattern the implementer copies (same approach the Phase-1 plan used for tag fixtures). Not a vague placeholder.

**Type consistency:** `AlbumYearSource` (config.rs) is the single source; `album_year`/`album_dates` signatures and the `(year<<9)` packing match across T1–T4. `Album` gains `original_date`/`release_date: u32` in T3 and is consumed with the same names in T4. `TrackView` fields `original_date`/`release_date: u32` are set in T1's projection and read in T3's `group_albums`.
