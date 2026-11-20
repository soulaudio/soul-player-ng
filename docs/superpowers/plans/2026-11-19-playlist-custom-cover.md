# Custom Playlist Cover Upload Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Let a user set/remove a custom cover image on a playlist, overriding the auto mosaic, honored on the list page + detail hero. App-only.

**Architecture:** Store copies the picked image into the playlist folder as `cover.<ext>` and records it in `playlist.toml`'s reserved `custom_cover` field. A path-keyed image cache renders it. Precedence: custom > mosaic > single > placeholder. The file dialog is Rune's `tx.open_file`.

**Tech Stack:** Rust; `app/rune` (nightly from CWD); Rune immediate-mode UI + `ImageCache`; rust-i18n.

## Global Constraints

- Modify ONLY `app/rune` (`playlists/{store,cover,mod}.rs`, `views/{playlists,playlist_detail,detail_page}.rs`, `main.rs`, `locales/app.yml`, `i18n_tests.rs`, tests). NO backend/host/core.
- NO em-dashes (grep `\x{2014}` added lines before commit). NO attribution in commits.
- All user-facing strings in 6 locales (en/de/es/zh/cs/ja).
- Precedence everywhere a playlist cover renders: `custom_cover` (file exists) > mosaic > single > placeholder.
- TDD; `cargo clippy -p soul-player-rune --all-targets -- -D warnings`; slow compile, patient timeouts, no background+poll.
- Reuse the `config::use_test_config_dir()` isolation for store tests, and the scripted-`tx.open_file` dialog harness (used by the EQ/profile-import tests in `main.rs`) for the dialog UI test.

---

## Task 1: Store — set / clear / resolve custom cover

**Files:** Modify `app/rune/src/playlists/store.rs` (+ `mod.rs` re-exports if needed). Test: inline in `store.rs`.

**Interfaces (produces):**
```rust
impl PlaylistStore {
    /// Validate (image ext + size cap), copy `src` into the playlist folder as
    /// `cover.<ext>` (removing any prior cover.*), set custom_cover, persist.
    /// Returns true if applied.
    pub fn set_custom_cover(&mut self, id: PlaylistId, src: &Path) -> bool;
    /// Delete the cover file, clear custom_cover, persist.
    pub fn clear_custom_cover(&mut self, id: PlaylistId);
    /// Absolute path to the current cover file, if set AND the file exists.
    pub fn custom_cover_path(&self, id: PlaylistId) -> Option<PathBuf>;
}
// PlaylistSummary gains: pub custom_cover: Option<PathBuf>  (populated by list())
```
Accepted extensions (lowercased): `jpg jpeg png webp gif`. Size cap: 16 MiB. `cover.<ext>` where `<ext>` is the source's lowercased extension.

- [ ] **Step 1: Failing tests**
```rust
#[test] fn set_custom_cover_copies_file_sets_field_and_persists() {
    let _g = config::use_test_config_dir(); reset();
    let mut s = PlaylistStore::load(); let id = s.create("P");
    let img = write_temp_png();                      // helper: a tiny real .png in a temp dir
    assert!(s.set_custom_cover(id, &img));
    let p = PlaylistStore::load();                   // fresh from disk
    let cover = p.custom_cover_path(id).expect("cover path resolves");
    assert!(cover.exists() && cover.file_name().unwrap().to_string_lossy().starts_with("cover."));
    assert!(p.list().iter().find(|s| s.id==id).unwrap().custom_cover.is_some());
}
#[test] fn rejects_non_image_and_oversize() {
    let _g = config::use_test_config_dir(); reset();
    let mut s = PlaylistStore::load(); let id = s.create("P");
    assert!(!s.set_custom_cover(id, &write_temp_file("x.txt", b"nope")));
    assert!(s.custom_cover_path(id).is_none());
}
#[test] fn clear_removes_file_and_field() {
    let _g = config::use_test_config_dir(); reset();
    let mut s = PlaylistStore::load(); let id = s.create("P");
    s.set_custom_cover(id, &write_temp_png());
    let path = s.custom_cover_path(id).unwrap();
    s.clear_custom_cover(id);
    assert!(!path.exists());
    assert!(PlaylistStore::load().custom_cover_path(id).is_none());
}
```
- [ ] **Step 2-4:** fail -> implement -> pass -> full store suite. Replacing a cover removes the old `cover.*` first (don't orphan a different-extension file). `custom_cover_path` returns None if the field is set but the file is missing (stale).
- [ ] **Step 5: Commit** `feat(playlists): store set/clear/resolve a custom cover file`.

---

## Task 2: Cover cache + Custom variant + list-page rendering

**Files:** Modify `app/rune/src/playlists/cover.rs` (+ `views/playlists.rs`), `app/rune/src/main.rs` (State: a cover-image cache + wire its Waker in `AppSpec::setup`). Test: inline.

**Interfaces:**
```rust
pub enum PlaylistCover { Mosaic(Vec<u32>), Single(u32), Placeholder, Custom(PathBuf) }
```
Add a path-keyed image cache (mirror `player::ArtCache`: `ImageCache` + a `RefCell<HashMap<PathBuf, Image>>`; `load_mime(fs::read(path), mime_from_ext, px, scale)`), owned by `State`, Waker wired in `AppSpec::setup`. OR extend `ArtCache` with a `image_for_path(&Path)` method and reuse it.

The list page chooses the cover: if `summary.custom_cover` is `Some(path)` -> `Custom(path)`, else the existing `select_cover` result. `playlist_cover_el` renders `Custom(path)` as a square `Image` via the cache, falling back to the placeholder if the load fails.

- [ ] **Step 1: Failing test**
```rust
#[test] fn custom_cover_takes_precedence_over_mosaic() {
    // playlist_cover (or the list chooser) returns Custom when custom_cover is Some,
    // even if member tracks would form a mosaic.
    let cover = choose_playlist_cover(Some(PathBuf::from("/x/cover.png")), &some_resolved_ids(), |_| true);
    assert!(matches!(cover, PlaylistCover::Custom(_)));
}
#[test] fn falls_back_to_mosaic_without_custom() {
    let cover = choose_playlist_cover(None, &four_distinct_albums(), |_| true);
    assert!(matches!(cover, PlaylistCover::Mosaic(_)));
}
```
(Factor a pure `choose_playlist_cover(custom: Option<PathBuf>, resolved, has_art) -> PlaylistCover` so precedence is unit-testable without a Runtime.)
- [ ] **Step 2-4:** fail -> implement -> pass -> clippy --all-targets -> fmt. Rendering `Custom` in `playlist_cover_el` must not panic when the file is unreadable (fall back to placeholder).
- [ ] **Step 5: Commit** `feat(playlists): render a custom cover on the Playlists list (precedence over mosaic)`.

---

## Task 3: HeroArt::Image + detail hero renders the custom cover

**Files:** Modify `app/rune/src/views/detail_page.rs` (`HeroArt` + `hero_artwork`), `app/rune/src/views/playlist_detail.rs` (hero selection). Test: inline.

**Interfaces:** `HeroArt` gains `Image(PathBuf)`. `hero_artwork` renders it via the cover cache (same as Task 2's list rendering), falling back to the icon placeholder on load failure. `playlist_detail` selects `HeroArt::Image(path)` when `s.playlists.custom_cover_path(open_id)` is `Some`, else the existing cover-track/`HeroArt::Icon` logic.

- [ ] **Step 1: Failing test** (full-app `main.rs` harness, since the hero renders in the detail page): a playlist with a custom cover set shows the hero image element (keyed, e.g. `playlist-hero-image`); without one, it shows the mosaic/icon. (Presence assertions; model on existing detail render tests.)
- [ ] **Step 2-4:** fail -> implement -> pass -> full suite -> clippy -> fmt. Confirm the 3 other `HeroArt` users (album/artist/genre detail) are unaffected by the new variant (exhaustive match arms updated).
- [ ] **Step 5: Commit** `feat(playlists): HeroArt::Image renders a playlist's custom cover in the detail hero`.

---

## Task 4: Set / Remove cover UI (file dialog)

**Files:** Modify `app/rune/src/views/playlist_detail.rs`, `app/rune/locales/app.yml`, `app/rune/src/i18n_tests.rs` (scan list). Test: inline, via the scripted-dialog harness.

**Interfaces:** "Set cover" control (key `playlist-set-cover`) -> `tx.open_file(FileDialogOptions { title: t!("playlists.setCoverDialog"), ..Default::default() }.filter("Images", &["jpg","jpeg","png","webp","gif"]), |s, result, _tx| { if let Ok(Some(path)) = result { s.playlists.set_custom_cover(open_id, &path); } })`. "Remove cover" control (key `playlist-remove-cover`), shown only when `custom_cover_path(open_id).is_some()`, -> `s.playlists.clear_custom_cover(open_id)`.

- [ ] **Step 1: Failing tests** (reuse the EQ/profile-import scripted-`tx.open_file` dialog test pattern from `main.rs` so a picked path flows through): pressing "Set cover" with a scripted picked image path results in `custom_cover_path(id).is_some()`; "Remove cover" is absent before a cover is set and present after; pressing it clears the cover.
- [ ] **Step 2-4:** fail -> implement -> pass -> full suite -> clippy --all-targets -> fmt.
- [ ] **Step 5: Locale** `playlists.setCover` / `playlists.removeCover` / `playlists.setCoverDialog` in 6 locales; add `playlist_detail.rs` keys to the completeness check.
- [ ] **Step 6: Commit** `feat(playlists): set/remove a custom cover from the playlist detail page`.

---

## Task 5: e2e + locale completeness

**Files:** Modify `app/rune/src/main.rs` (e2e), `app/rune/src/i18n_tests.rs`.

- [ ] **Step 1: e2e** (full app, scripted dialog + `config::use_test_config_dir`): open a playlist -> Set cover (scripted image path) -> the detail hero renders the custom image element AND the list row's thumbnail does too -> Remove cover -> reverts to mosaic/placeholder. Assert the cover file is created then deleted on disk.
- [ ] **Step 2-4:** fail -> pass -> full suite -> clippy -> fmt.
- [ ] **Step 5:** extend the playlists locale-completeness test with the new keys.
- [ ] **Step 6: Commit** `test(playlists): e2e set -> render -> remove a custom cover`.
