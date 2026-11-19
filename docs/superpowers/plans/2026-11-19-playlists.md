# Playlists Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship user-authored playlists in the Rune desktop head (create / rename / delete / add / remove / reorder / persist), a Playlists nav page with live 2x2 cover mosaics, Play-All and add-to-queue, and offline-safe rows — storing durable locators (not volatile ids) so playlists survive rescans, with the Phase 2 DAP-export seam left compatible.

**Architecture:** Read/write split. The editable store lives in `app/rune` (per-playlist folders under `.soul/playlists/`, a serde-TOML manifest for cheap listing). Tracks are referenced by a durable `TrackLocator` + metadata snapshot, resolved to the current volatile track id only at use time via two new backend/host methods. Playback reuses the existing `PlayContext`/`PlayNext`/`Enqueue` commands. `soul-audio-core` is NOT modified in Phase 1.

**Tech Stack:** Rust; three `-ng` cargo workspaces (`soul-player-backend` + `soul-player-host` + `app/rune`); serde + toml; Rune immediate-mode GPU UI; rust-i18n.

## Global Constraints

- **Phase 1 modifies only** `crates/soul-player-backend`, `crates/soul-player-host`, `app/rune`. **`soul-audio-core` is NOT modified** (no rev bump, no core worktree).
- Three workspaces, three lockfiles: run cargo from the correct workspace dir. `app/rune` builds on nightly-2026-11-10 (auto-selected from CWD `app/rune/`).
- Commit messages: **no attribution** (no Claude/Anthropic/AI/Co-Authored-By — the commit-msg hook rejects them).
- **No em-dashes** in UI copy, prose, or commits (use commas / colons / parens / middots); do not mass-rewrite deep code comments.
- Every user-facing string localized in all 6 locales: en, de, es, zh, cs, ja (`app/rune/locales/app.yml`, single-quoted values).
- A playlist stores a **durable locator + snapshot, never a volatile track id**. `State.open_playlist` keys on the stable playlist **id**, never the name.
- The themed in-app overlay is the only dropdown mechanism (arch test bans `PopupMenu::picker`); reuse `views::dropdown` / `El::anchored` / `El::modal`.
- TDD throughout: write the failing test, watch it fail, minimal code to pass. Frequent commits. DRY, YAGNI.
- Config/test isolation: tests that touch the store must use the test config dir (`soul_player_dir()` returns a temp dir in test builds; follow the `CONFIG_TEST_LOCK` / `use_test_config_dir` pattern already in `config.rs`).

---

## File Structure

**backend (`crates/soul-player-backend/src/`)**
- `types.rs` — add `TrackLocator` enum + `TrackSnapshot` struct (serde DTOs).
- `library.rs` — add `Library::locator_of` + `Library::resolve_locators`.

**host (`crates/soul-player-host/src/`)**
- `lib.rs` — `PlayerHandle::locator_of` / `resolve_locators` pass-throughs; re-export `TrackLocator` / `TrackSnapshot`.

**app (`app/rune/src/`)**
- `playlists/mod.rs` — module root; re-exports.
- `playlists/model.rs` — `Playlist`, `PlaylistEntry`, `slugify`, stable-id types.
- `playlists/store.rs` — `playlist_store_path()`, per-folder load/save, `index.toml` manifest, CRUD.
- `playlists/cover.rs` — mosaic cover-id selection + memoization.
- `views/playlists.rs` — Playlists list page.
- `views/playlist_detail.rs` — single-playlist detail page.
- `views/add_to_playlist.rs` — the "Add to playlist" overlay.
- `main.rs` — `Route::Playlists`/`Playlist`, `State` fields, nav wiring, view dispatch.
- `views/sidebar.rs` — `NAV_DEFS` entry, `nav_label`/`nav_active` arms.
- `locales/app.yml` — all new keys in 6 locales.

---

## Task 1: TrackLocator + TrackSnapshot DTOs

**Files:**
- Modify: `crates/soul-player-backend/src/types.rs`
- Modify: `crates/soul-player-host/src/lib.rs` (re-export)
- Test: inline `#[cfg(test)]` in `types.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  #[serde(tag = "kind", rename_all = "lowercase")]
  pub enum TrackLocator {
      Local  { path: String },            // absolute filesystem path, normalized
      Remote { server: String, rel: String },
  }

  #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
  pub struct TrackSnapshot {
      pub title: String,
      pub artist: String,
      pub album: String,
      pub duration_secs: u32,
      #[serde(default, skip_serializing_if = "Option::is_none")] pub track_no: Option<u32>,
      #[serde(default, skip_serializing_if = "Option::is_none")] pub disc_no: Option<u32>,
  }

  impl TrackLocator {
      /// Normalized comparison key (lowercased on Windows, forward slashes).
      pub fn key(&self) -> String;
  }
  ```

- [ ] **Step 1: Write the failing test** (in `types.rs`)

```rust
#[cfg(test)]
mod locator_tests {
    use super::*;

    #[test]
    fn locator_serde_roundtrips_both_variants() {
        let local = TrackLocator::Local { path: "D:/Music/a.flac".into() };
        let remote = TrackLocator::Remote { server: "https://h".into(), rel: "x/y.flac".into() };
        for loc in [local, remote] {
            let s = toml::to_string(&Wrap { loc: loc.clone() }).unwrap();
            let back: Wrap = toml::from_str(&s).unwrap();
            assert_eq!(back.loc, loc);
        }
    }

    #[test]
    fn local_key_is_case_and_separator_normalized() {
        let a = TrackLocator::Local { path: r"D:\Music\A.flac".into() };
        let b = TrackLocator::Local { path: "d:/music/a.flac".into() };
        assert_eq!(a.key(), b.key());
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrap { loc: TrackLocator }
}
```

- [ ] **Step 2: Run, verify it fails** — `cargo test -p soul-player-backend locator_` → FAIL (types missing).
- [ ] **Step 3: Implement** the two types + `key()`. `key()`: replace `\\` with `/`, and (on Windows semantics, but do it unconditionally for determinism) lowercase. Keep it dependency-free.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Re-export** `TrackLocator` + `TrackSnapshot` from `soul-player-host` `lib.rs` (`pub use soul_player_backend::types::{TrackLocator, TrackSnapshot};` — match the existing re-export style). Build host: `cargo build -p soul-player-host`.
- [ ] **Step 6: Commit** `feat(playlists): TrackLocator + TrackSnapshot durable-reference DTOs`.

---

## Task 2: Library::locator_of + resolve_locators

**Files:**
- Modify: `crates/soul-player-backend/src/library.rs`
- Test: existing `#[cfg(test)]` module in `library.rs` (reuse its `tmpdir`, `wav`, `Library::scan` helpers).

**Interfaces:**
- Consumes: `TrackLocator` (Task 1); existing `Library::locate(id)->Option<Located>`, `path_of(id)`, and the catalog walk used by `views()`.
- Produces:
  ```rust
  impl Library {
      /// Durable locator for a current track id (Local for local sources).
      pub fn locator_of(&self, id: u32) -> Option<TrackLocator>;
      /// Resolve durable locators to current ids (None where unresolved). Builds
      /// a path->id reverse map from the current catalog once per call.
      pub fn resolve_locators(&self, locs: &[TrackLocator]) -> Vec<Option<u32>>;
  }
  ```

**Scope note:** Phase 1 implements **exact-locator (Local path)** resolution — step 1 of the spec's algorithm. Remote `locator_of` returns a best-effort `Remote { server, rel }` when that info is cheaply available from the host's remote mapping, else `None`; remote resolution matches on `(server, rel)` when the catalog exposes it, else yields `None` (renders as unavailable). MBID / single-metadata fallbacks (steps 2-3) and the root-prefix fast path are **deferred** (listed as follow-ups) — do NOT build them here.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn locator_of_returns_the_absolute_path_for_a_local_track() {
    let dir = tmpdir();
    wav(&dir, "a.wav"); wav(&dir, "b.wav");
    let lib = Library::scan(&[dir.path()]).unwrap();
    let id = lib.views()[0].id;
    let loc = lib.locator_of(id).unwrap();
    match loc { TrackLocator::Local { path } => assert!(path.ends_with("a.wav")),
                _ => panic!("expected local") }
}

#[test]
fn resolve_locators_survives_a_rescan_that_renumbers_ids() {
    let dir = tmpdir();
    wav(&dir, "b.wav"); wav(&dir, "c.wav");
    let lib = Library::scan(&[dir.path()]).unwrap();
    // capture a durable locator for the track currently named b.wav
    let b_id = lib.views().iter().find(|v| /* its path ends b.wav via locator_of */
        matches!(lib.locator_of(v.id), Some(TrackLocator::Local{ path }) if path.ends_with("b.wav"))).unwrap().id;
    let loc = lib.locator_of(b_id).unwrap();
    // insert an alphabetically-earlier file -> b.wav's local id shifts
    wav(&dir, "a.wav");
    let lib2 = Library::scan(&[dir.path()]).unwrap();
    let resolved = lib2.resolve_locators(&[loc.clone()])[0].expect("resolves after rescan");
    // the resolved id must still map back to b.wav
    assert!(matches!(lib2.locator_of(resolved), Some(TrackLocator::Local{ path }) if path.ends_with("b.wav")));
    // and the raw id value should differ from before (renumbered)
    assert_ne!(resolved, b_id);
}

#[test]
fn resolve_locators_returns_none_for_a_missing_path() {
    let dir = tmpdir(); wav(&dir, "a.wav");
    let lib = Library::scan(&[dir.path()]).unwrap();
    let miss = TrackLocator::Local { path: "D:/nope/gone.flac".into() };
    assert_eq!(lib.resolve_locators(&[miss]), vec![None]);
}
```

- [ ] **Step 2: Run, verify fail** (`cargo test -p soul-player-backend locator_of resolve_locators`).
- [ ] **Step 3: Implement.** `locator_of`: use `locate(id)`/`path_of(id)` to build an absolute path -> `TrackLocator::Local { path }` (normalized to forward slashes). `resolve_locators`: walk all current track ids once (same iteration as `views()`), build `HashMap<String /*locator.key()*/, u32>`, then map each input via `.key()`. Reuse `path_of` for the reverse map so keys match `locator_of` output exactly.
- [ ] **Step 4: Run, verify pass. Run full backend suite** `cargo test -p soul-player-backend` (no regressions).
- [ ] **Step 5: Commit** `feat(playlists): Library locator_of + resolve_locators (rescan-stable)`.

---

## Task 3: PlayerHandle pass-throughs

**Files:**
- Modify: `crates/soul-player-host/src/lib.rs`
- Test: inline host test if the harness allows building a `PlayerHandle` over a temp library; otherwise a compile-level test asserting the methods exist and delegate.

**Interfaces:**
- Produces:
  ```rust
  impl PlayerHandle {
      pub fn locator_of(&self, id: u32) -> Option<TrackLocator>;
      pub fn resolve_locators(&self, locs: &[TrackLocator]) -> Vec<Option<u32>>;
  }
  ```

- [ ] **Step 1: Write the failing test.** If `PlayerHandle` can wrap a scanned temp library in tests (check existing host tests for the pattern), assert `host.locator_of(id)` round-trips and `host.resolve_locators` matches the backend. If host tests cannot construct a library, add a thin test in the app layer later (Task 5) and here just assert the delegation compiles + forwards (a unit test constructing the inner `Library` directly where possible).
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** the two pass-throughs delegating to the wrapped `Library` (match how `views_by`/`art` reach the library behind its `Mutex`).
- [ ] **Step 4: Run, verify pass.** `cargo test -p soul-player-host`.
- [ ] **Step 5: Commit** `feat(playlists): host PlayerHandle locator pass-throughs`.

---

## Task 4: Playlist model + slug + stable id

**Files:**
- Create: `app/rune/src/playlists/mod.rs`, `app/rune/src/playlists/model.rs`
- Modify: `app/rune/src/main.rs` (add `mod playlists;`)
- Test: inline `#[cfg(test)]` in `model.rs`

**Interfaces:**
- Consumes: `TrackLocator`, `TrackSnapshot` (via host re-export).
- Produces:
  ```rust
  pub type PlaylistId = u64;

  #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
  pub struct PlaylistEntry {
      pub locator: TrackLocator,
      pub snapshot: TrackSnapshot,
      #[serde(default, skip_serializing_if = "Option::is_none")] pub mbid: Option<String>,
  }

  #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
  pub struct Playlist {
      pub id: PlaylistId,
      pub name: String,
      #[serde(default, skip_serializing_if = "Option::is_none")] pub custom_cover: Option<String>,
      #[serde(default)] pub entries: Vec<PlaylistEntry>,
  }

  impl Playlist {
      pub fn move_entry(&mut self, from: usize, to: usize) -> bool;  // clamp; no-op if out of range
      pub fn remove_entry(&mut self, index: usize) -> Option<PlaylistEntry>;
  }

  /// Filesystem-safe, legible slug from a display name (never the identity).
  pub fn slugify(name: &str) -> String;   // lowercased, [^a-z0-9]+ -> '-', trim '-', <=40 chars, "" if empty
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn slugify_sanitizes_illegal_and_unicode() {
    assert_eq!(slugify("Chill / Vibes: 2024!"), "chill-vibes-2024");
    assert_eq!(slugify("   "), "");
    assert_eq!(slugify("CON"), "con"); // reserved-name handling lives in the folder builder, not here
    assert!(slugify(&"x".repeat(100)).len() <= 40);
}
#[test] fn move_entry_reorders() {
    let mut p = pl(&["a","b","c"]);
    assert!(p.move_entry(0, 2));
    assert_eq!(titles(&p), ["b","c","a"]);
    assert!(!p.move_entry(9, 0));
}
#[test] fn remove_entry_pulls_one() {
    let mut p = pl(&["a","b","c"]);
    assert_eq!(p.remove_entry(1).unwrap().snapshot.title, "b");
    assert_eq!(titles(&p), ["a","c"]);
}
#[test] fn playlist_toml_roundtrips() {
    let p = pl(&["a","b"]);
    let back: Playlist = toml::from_str(&toml::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
}
```
(`pl`/`titles` are local helpers building `Playlist` from title strings with dummy `Local` locators.)

- [ ] **Step 2-4: fail → implement → pass.** `slugify`: lowercase, map any run of non-`[a-z0-9]` to a single `-`, trim leading/trailing `-`, truncate to 40 (trim trailing `-` again).
- [ ] **Step 5: Commit** `feat(playlists): playlist model, slugify, reorder/remove`.

---

## Task 5: On-disk store + manifest

**Files:**
- Create: `app/rune/src/playlists/store.rs`
- Modify: `app/rune/src/playlists/mod.rs`, `app/rune/src/config.rs` (expose a `managed_library_root()` lookup if not present; else read config)
- Test: inline `#[cfg(test)]` in `store.rs` using the test config dir + `CONFIG_TEST_LOCK`.

**Interfaces:**
- Consumes: `Playlist`, `slugify`, `config::soul_player_dir()`, managed-library config.
- Produces:
  ```rust
  pub fn playlist_store_path() -> PathBuf;   // <managed-lib>/.soul/playlists OR soul_player_dir()/playlists

  #[derive(Default)]
  pub struct PlaylistStore { /* loaded playlists + next_id, keyed by id */ }

  impl PlaylistStore {
      pub fn load() -> Self;                              // scans folders; rebuilds manifest if stale
      pub fn create(&mut self, name: &str) -> PlaylistId; // persists folder + playlist.toml + manifest
      pub fn rename(&mut self, id: PlaylistId, name: &str);
      pub fn delete(&mut self, id: PlaylistId);
      pub fn add_entry(&mut self, id: PlaylistId, entry: PlaylistEntry);
      pub fn remove_entry(&mut self, id: PlaylistId, index: usize);
      pub fn move_entry(&mut self, id: PlaylistId, from: usize, to: usize);
      pub fn get(&self, id: PlaylistId) -> Option<&Playlist>;
      pub fn list(&self) -> Vec<PlaylistSummary>;         // from manifest order: id, name, count, cover locators
  }

  pub struct PlaylistSummary { pub id: PlaylistId, pub name: String, pub count: usize, pub cover: Vec<TrackLocator> }
  ```

Manifest `index.toml`: `{ next_id, playlists: [{ id, slug, order, name, count, cover: [TrackLocator; <=4] }] }`. Each mutation: update the in-memory playlist, write that one `playlist.toml`, rewrite `index.toml`. Folder name `<slug>-<id>` (or `<id>` when slug empty); dedupe reserved/empty via the id suffix. Cover locators in the manifest = the first <=4 distinct-album entries' locators (computed by Task 6's selector; for Task 5 use "first <=4 entries' locators" and let Task 6 refine).

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn create_add_reload_roundtrips() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let mut s = PlaylistStore::load();
    let id = s.create("Road Trip");
    s.add_entry(id, entry("song one"));
    s.add_entry(id, entry("song two"));
    let s2 = PlaylistStore::load();                   // fresh read from disk
    assert_eq!(s2.get(id).unwrap().entries.len(), 2);
    assert_eq!(s2.list()[0].name, "Road Trip");
    assert_eq!(s2.list()[0].count, 2);
}
#[test] fn editing_one_playlist_does_not_rewrite_another() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let mut s = PlaylistStore::load();
    let a = s.create("A"); let b = s.create("B");
    let b_file = playlist_file_of(b);                 // test helper: locate B's playlist.toml
    let b_mtime = fs::metadata(&b_file).unwrap().modified().unwrap();
    s.add_entry(a, entry("x"));                        // mutate A only
    assert_eq!(fs::metadata(&b_file).unwrap().modified().unwrap(), b_mtime);
}
#[test] fn manifest_rebuilds_when_deleted() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let mut s = PlaylistStore::load();
    let id = s.create("Keeper"); s.add_entry(id, entry("x"));
    fs::remove_file(playlist_store_path().join("index.toml")).unwrap();  // simulate stale/missing manifest
    let s2 = PlaylistStore::load();
    assert_eq!(s2.get(id).unwrap().name, "Keeper");   // recovered by scanning folders
}
#[test] fn duplicate_names_get_distinct_folders() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let mut s = PlaylistStore::load();
    let a = s.create("Chill"); let b = s.create("Chill");
    assert_ne!(folder_of(a), folder_of(b));
    assert_eq!(s.list().len(), 2);
}
#[test] fn rename_keeps_id() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let mut s = PlaylistStore::load();
    let id = s.create("Old"); s.rename(id, "New");
    assert_eq!(PlaylistStore::load().get(id).unwrap().name, "New");
}
```

- [ ] **Step 2-4:** fail → implement → pass. Be careful: `load()` must be robust to a missing/garbled manifest (rebuild from folders) and a missing store dir (empty store). `next_id` persists in the manifest and only increments.
- [ ] **Step 5: Commit** `feat(playlists): on-disk per-folder store + manifest`.

---

## Task 6: Cover mosaic selection

**Files:**
- Create: `app/rune/src/playlists/cover.rs`
- Modify: `app/rune/src/playlists/mod.rs`; wire Task 5's manifest `cover` to use this selector.
- Test: inline `#[cfg(test)]` in `cover.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum PlaylistCover { Mosaic(Vec<u32>), Single(u32), Placeholder }  // ids resolved against the live library
  /// Pick up to 4 distinct-album cover track ids from resolved entries, in order.
  pub fn select_cover(resolved: &[(u32, AlbumKey)], has_art: impl Fn(u32)->bool) -> PlaylistCover;
  ```
  `AlbumKey` = whatever cheap album-identity the app already exposes (album name string, or the album id); dedupe on it, else fall back to the art presence.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn four_distinct_albums_make_a_mosaic() {
    let cov = select_cover(&ids_albums(&[(1,"A"),(2,"B"),(3,"C"),(4,"D"),(5,"E")]), |_| true);
    assert_eq!(cov, PlaylistCover::Mosaic(vec![1,2,3,4]));
}
#[test] fn duplicate_albums_are_skipped() {
    let cov = select_cover(&ids_albums(&[(1,"A"),(2,"A"),(3,"B")]), |_| true);
    assert_eq!(cov, PlaylistCover::Single(1)); // only 2 distinct -> single
}
#[test] fn one_to_three_distinct_is_single_cover() {
    let cov = select_cover(&ids_albums(&[(7,"A"),(8,"B"),(9,"C")]), |_| true);
    assert_eq!(cov, PlaylistCover::Single(7));
}
#[test] fn no_art_is_placeholder() {
    let cov = select_cover(&ids_albums(&[(1,"A"),(2,"B")]), |_| false);
    assert_eq!(cov, PlaylistCover::Placeholder);
}
```

- [ ] **Step 2-4:** fail → implement → pass. Skip ids where `has_art(id)` is false before counting distinct albums.
- [ ] **Step 5:** Add memoization doc note: callers key the result on a hash of the entry-locator list (recompute only when membership changes). Implement the hash helper `pub fn entries_fingerprint(p: &Playlist) -> u64`.
- [ ] **Step 6: Commit** `feat(playlists): cover-mosaic selection (first 4 distinct, single/placeholder fallback)`.

---

## Task 7: State + nav routes + sidebar entry

**Files:**
- Modify: `app/rune/src/main.rs` (Route enum, State fields, navigate_to/go_back/NavEntry, view dispatch, page title, `set_*` helpers), `app/rune/src/views/sidebar.rs` (NAV_DEFS, nav_label, nav_active).
- Test: inline tests in `main.rs` (Runtime-based, following existing nav tests).

**Interfaces:**
- Consumes: `PlaylistStore`, `PlaylistId`.
- Produces: `Route::Playlists`, `Route::Playlist`; `State.playlists: PlaylistStore`; `State.open_playlist: Option<PlaylistId>`; `navigate_to(.., Route::Playlist, .. open_playlist ..)`; a `sidebar` "playlists" `NavDef`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn sidebar_has_a_playlists_destination() {
    assert!(sidebar::NAV_DEFS.iter().any(|d| d.id == "playlists"));
}
#[test] fn opening_a_playlist_sets_open_playlist_and_back_returns() {
    let rt = test_runtime_with_one_playlist(); // helper: store with one playlist "P" (id X)
    rt.update(|st,_| navigate_to(st, Route::Playlist, None, None, None, Some(/*id X*/)));
    rt.read(|st| { assert_eq!(st.route, Route::Playlist); assert_eq!(st.open_playlist, Some(/*X*/)); });
    rt.update(|st,_| go_back(st));
    rt.read(|st| assert_eq!(st.route, Route::Playlists));
}
```
(Extend `NavEntry` with `open_playlist`; extend `navigate_to`'s signature with the new `open_playlist` param and update all existing call sites to pass `None`.)

- [ ] **Step 2-4:** fail → implement → pass. Add the view-dispatch arms (`Route::Playlists => views::playlists::page(..)`, `Route::Playlist => views::playlist_detail::page(..)` — stubs returning an empty `El` until Tasks 8/9), page-title arms, `nav_active` so `Route::Playlist` lights the `playlists` tab, and `nav_hidden` id support. Add `mod playlist_detail;`/`mod playlists;` under `views`.
- [ ] **Step 5: Commit** `feat(playlists): routes, state, sidebar nav entry`.

---

## Task 8: Playlists list page

**Files:**
- Create: `app/rune/src/views/playlists.rs`
- Modify: `app/rune/src/main.rs` (dispatch to the real page), `app/rune/locales/app.yml`
- Test: inline Runtime render tests.

**Interfaces:**
- Consumes: `PlaylistStore::list()`, `select_cover`, `navigate_to`, `host.resolve_locators` (to turn summary cover locators into ids for the mosaic), `host.art(id)`.
- Produces: `pub fn page(s, p) -> El`; a `New playlist` action calling `s.playlists.create(..)`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn playlists_page_lists_names_and_counts() {
    let rt = runtime_with_playlists(&[("Road Trip", 3), ("Focus", 0)]);
    rt.update(|st,_| st.route = Route::Playlists);
    assert!(shows(&rt, "Road Trip"));
    assert!(shows(&rt, "Focus"));
}
#[test] fn new_playlist_creates_and_persists() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let rt = empty_runtime();
    rt.update(|st,_| { st.route = Route::Playlists; });
    trigger_new_playlist(&rt, "Fresh");     // helper driving the inline new-playlist field + confirm
    assert!(shows(&rt, "Fresh"));
    assert_eq!(PlaylistStore::load().list().len(), 1);
}
```

- [ ] **Step 2-4:** fail → implement → pass. Row = cover thumbnail (via a `playlist_cover` render helper that paints `PlaylistCover` using `host.art(id)` for `Single`/`Mosaic` and a gradient for `Placeholder`), name, count; click -> `navigate_to(.., Route::Playlist, .., Some(id))`. New-playlist affordance uses inline `TextEdit` (focus via `.focused(true)`), Enter/confirm -> `create` -> navigate into it. All strings localized.
- [ ] **Step 5: Add locale keys** (`playlists.title`, `playlists.new`, `playlists.newPlaceholder`, `playlists.empty`, `playlists.trackCount` with count interpolation) in all 6 locales.
- [ ] **Step 6: Commit** `feat(playlists): Playlists list page + new-playlist`.

---

## Task 9: Playlist detail page

**Files:**
- Create: `app/rune/src/views/playlist_detail.rs`
- Modify: `app/rune/src/main.rs` (dispatch), `app/rune/locales/app.yml`
- Test: inline Runtime render tests.

**Interfaces:**
- Consumes: `PlaylistStore::get`, `host.resolve_locators`, `host.art`, shared `detail_hero` / `play_all_pill` / `detail_track_row`, `Command::PlayContext/PlayNext/Enqueue`, `s.playlists.{rename,delete,remove_entry,move_entry}`.
- Produces: `pub fn page(s, p) -> El`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn play_all_issues_playcontext_with_resolved_ids_in_order() {
    let rt = runtime_playlist_over_library(&["a.wav","b.wav"]); // playlist of 2 real tracks
    rt.update(|st,_| { st.route = Route::Playlist; st.open_playlist = Some(the_id); });
    click(&rt, "playlist-play-all");
    let cmd = last_command(&rt);
    assert!(matches!(cmd, Command::PlayContext { ids, start: 0 } if ids.len() == 2));
}
#[test] fn removing_a_row_updates_the_store() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let rt = runtime_playlist_over_library(&["a.wav","b.wav"]);
    open_playlist(&rt, the_id);
    click(&rt, "playlist-remove-0");
    rt.read(|st| assert_eq!(st.playlists.get(the_id).unwrap().entries.len(), 1));
    assert_eq!(PlaylistStore::load().get(the_id).unwrap().entries.len(), 1);
}
#[test] fn an_unavailable_track_renders_greyed_from_snapshot() {
    let rt = runtime_playlist_with_one_missing_entry("Ghost Song"); // locator resolves to None
    open_playlist(&rt, the_id);
    assert!(shows(&rt, "Ghost Song"));          // snapshot title still renders
    assert!(!row_is_playable(&rt, "playlist-row-0"));
}
```

- [ ] **Step 2-4:** fail → implement → pass. Resolve the playlist's locators once per render-build (or cache on open) via `host.resolve_locators`; rows for `Some(id)` are normal, rows for `None` render greyed from `entry.snapshot` and are non-interactive for play/drag. Controls: rename (inline text), delete (confirm), per-row remove (`playlist-remove-{i}`), move up/down (`playlist-up-{i}`/`playlist-down-{i}` -> `move_entry`). `play_all_pill` resolves (skipping `None`) and issues `PlayContext { ids, start: 0 }`.
- [ ] **Step 5: Add locale keys** (`playlists.playAll`, `playlists.rename`, `playlists.delete`, `playlists.deleteConfirm`, `playlists.removeTrack`, `playlists.moveUp`, `playlists.moveDown`, `playlists.unavailable`) in 6 locales.
- [ ] **Step 6: Commit** `feat(playlists): playlist detail page (play-all, remove, reorder, rename, delete)`.

---

## Task 10: Add-to-playlist overlay

**Files:**
- Create: `app/rune/src/views/add_to_playlist.rs`
- Modify: `app/rune/src/main.rs` (State: `add_to_playlist_for: Option<u32>` i.e. the track id pending add; open/close helpers), the track-row context menus in `views/library.rs` (+ `genres.rs`, `albums`/`detail_page.rs` track rows), `app/rune/locales/app.yml`
- Test: inline Runtime tests.

**Interfaces:**
- Consumes: `host.locator_of(id)`, the on-screen `TrackView` for the snapshot, `PlaylistStore::{list, add_entry, create}`.
- Produces: an overlay listing playlists + an inline "New playlist..." field; an `add_track_to_playlist(s, track_id, playlist_id)` helper that builds the `PlaylistEntry { locator: host.locator_of(id), snapshot: from TrackView, mbid: None }` and calls `add_entry`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn add_to_playlist_stores_locator_and_snapshot() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let rt = runtime_over_library(&["a.wav"]);
    let pid = rt.update(|st,_| st.playlists.create("Target"));
    let tid = rt.read(|st| st.library[0].id);
    rt.update(|st,_| add_track_to_playlist(st, tid, pid));
    let e = &PlaylistStore::load().get(pid).unwrap().entries[0];
    assert!(matches!(&e.locator, TrackLocator::Local{ path } if path.ends_with("a.wav")));
    assert_eq!(e.snapshot.title, rt.read(|st| st.library[0].title.clone()));
}
#[test] fn add_overlay_new_playlist_adds_to_the_new_one() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let rt = runtime_over_library(&["a.wav"]);
    let tid = rt.read(|st| st.library[0].id);
    open_add_overlay(&rt, tid);
    overlay_new_playlist(&rt, "Brand New");
    let s = PlaylistStore::load();
    let p = s.list().iter().find(|p| p.name == "Brand New").unwrap().id;
    assert_eq!(s.get(p).unwrap().entries.len(), 1);
}
```

- [ ] **Step 2-4:** fail → implement → pass. The overlay is the themed anchored/modal pattern (NOT native popup — it needs text input). Add an "Add to playlist" item to the existing track-row context menu(s) that sets `add_to_playlist_for = Some(track_id)` and opens the overlay. Keep snapshot construction in one helper so all call sites share it.
- [ ] **Step 5: Add locale keys** (`playlists.addTo`, `playlists.addToNew`, `playlists.added`) in 6 locales.
- [ ] **Step 6: Commit** `feat(playlists): add-to-playlist overlay + context-menu entry`.

---

## Task 11: Locale completeness + cross-locale overflow

**Files:**
- Modify: `app/rune/locales/app.yml` (fill any gaps)
- Test: reuse the existing cross-locale overflow test harness (`rt.elements()` across locales) + a key-completeness assertion.

- [ ] **Step 1: Write a failing test** asserting every `playlists.*` key present in `en` exists in de/es/zh/cs/ja, and a render of the Playlists + detail pages in each locale produces no overflow (reuse the existing overflow helper).
- [ ] **Step 2-4:** fail (if any key missing / overflow) → fill/adjust → pass.
- [ ] **Step 5: Commit** `test(playlists): locale completeness + cross-locale overflow`.

---

## Task 12: App render e2e

**Files:**
- Modify: `app/rune/src/main.rs` (inline e2e test alongside the existing ones)
- Test: one end-to-end render test.

**Interfaces:** Consumes everything above.

- [ ] **Step 1: Write the failing e2e**

```rust
#[test] fn e2e_new_playlist_add_track_then_play_all() {
    let _g = CONFIG_TEST_LOCK.lock(); use_test_config_dir();
    let rt = runtime_over_library(&["a.wav","b.wav"]);
    // create via the Playlists page
    rt.update(|st,_| st.route = Route::Playlists);
    trigger_new_playlist(&rt, "Mix");
    let pid = PlaylistStore::load().list()[0].id;
    // add both tracks via the overlay
    for i in 0..2 { let tid = rt.read(|st| st.library[i].id); rt.update(|st,_| add_track_to_playlist(st, tid, pid)); }
    // open it: shows on the page with the right count, Play All plays both in order
    rt.update(|st,_| navigate_to(st, Route::Playlist, None,None,None, Some(pid)));
    assert!(shows(&rt, "Mix"));
    click(&rt, "playlist-play-all");
    assert!(matches!(last_command(&rt), Command::PlayContext { ids, start: 0 } if ids.len()==2));
}
```

- [ ] **Step 2-4:** fail → (should pass once Tasks 1-10 are in; fix any integration gaps) → pass.
- [ ] **Step 5: Commit** `test(playlists): end-to-end new -> add -> play-all`.

---

## Deferred follow-ups (record in the ledger; not this plan)

- Resolution steps 2-3 (MBID + single-metadata relink) and the root-prefix move fast path.
- Phase 2 DAP export (bake locators -> `.soulidx` playlist table + device-sync seam; small isolated core change).
- Custom-cover **upload** UI (model field + folder layout already reserved).
- M3U8 write-only export; drag-reorder; smart playlists.
