# Heart -> Add-to-Playlist + Membership Manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make the now-playing heart open the add-to-playlist dialogue, and upgrade that dialogue into an add/remove membership manager, reusing the existing `PlaylistStore` + the already-shipped Playlists page.

**Architecture:** App-only (`app/rune`). The existing `views::add_to_playlist` overlay (opened via `open_add_to_playlist(s, track_id)` from the library context menu) gains membership-toggle rows; the dead now-playing heart becomes its second entry point. No backend/host/core change.

**Tech Stack:** Rust; `app/rune` (nightly auto-selected from CWD); Rune immediate-mode UI; rust-i18n.

## Global Constraints

- Modify ONLY `app/rune` (`views/add_to_playlist.rs`, `views/player.rs`, `main.rs` if needed, `locales/app.yml`, `i18n_tests.rs`, inline tests). NO backend/host/core.
- NO em-dashes anywhere (grep `\x{2014}` added lines before commit). NO attribution in commits.
- All user-facing strings in 6 locales (en/de/es/zh/cs/ja).
- The dialogue is the existing themed overlay, not a native picker.
- TDD; frequent commits; run `cargo clippy -p soul-player-rune --all-targets -- -D warnings` (CI lints tests). Slow compile; patient timeouts; no background+poll.

---

## Task 1: Dialogue becomes an add/remove membership manager

**Files:**
- Modify: `app/rune/src/views/add_to_playlist.rs`
- Modify: `app/rune/locales/app.yml` (a "Done" key if none exists)
- Test: inline tests in `add_to_playlist.rs`

**Interfaces:**
- Consumes: `PlaylistStore::{list, get, add_entry, remove_entry}`, existing `already_has(s, playlist_id, track_id)`, `add_track_to_playlist(s, track_id, playlist_id)`, `host.locator_of`.
- Produces:
  ```rust
  pub(crate) fn remove_track_from_playlist(s: &mut State, track_id: u32, playlist_id: PlaylistId);
  // finds the entry whose locator.key() == host.locator_of(track_id).key() and removes it; no-op if absent.
  pub(crate) fn toggle_track_in_playlist(s: &mut State, track_id: u32, playlist_id: PlaylistId);
  // already_has -> remove, else add. (The row on_press calls this.)
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn toggling_adds_then_removes_and_persists() {
    let _g = config::use_test_config_dir();
    let rt = runtime_over_library(&["a.wav"]);           // existing helper
    let pid = rt.update(|st,_| st.playlists.create("P"));
    let tid = rt.read(|st| st.library[0].id);
    rt.update(|st,_| toggle_track_in_playlist(st, tid, pid));   // add
    assert_eq!(PlaylistStore::load().get(pid).unwrap().entries.len(), 1);
    rt.update(|st,_| toggle_track_in_playlist(st, tid, pid));   // remove
    assert_eq!(PlaylistStore::load().get(pid).unwrap().entries.len(), 0);
}
#[test] fn remove_is_a_no_op_when_absent() {
    let _g = config::use_test_config_dir();
    let rt = runtime_over_library(&["a.wav"]);
    let pid = rt.update(|st,_| st.playlists.create("P"));
    let tid = rt.read(|st| st.library[0].id);
    rt.update(|st,_| remove_track_from_playlist(st, tid, pid)); // not present
    assert_eq!(PlaylistStore::load().get(pid).unwrap().entries.len(), 0);
}
#[test] fn overlay_stays_open_after_a_toggle() {
    let _g = config::use_test_config_dir();
    let rt = runtime_over_library(&["a.wav"]);
    let pid = rt.update(|st,_| st.playlists.create("P"));
    let tid = rt.read(|st| st.library[0].id);
    rt.update(|st,_| crate::open_add_to_playlist(st, tid));
    // toggling a row must NOT close the overlay (membership manager, not a picker)
    toggle_row(&rt, pid);                 // helper: press the playlist row
    rt.read(|st| assert_eq!(st.add_to_playlist_for, Some(tid)));
}
```

- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement.** Add `remove_track_from_playlist` (resolve `host.locator_of(track_id)`, find index in `store.get(pid).entries` by `locator.key()` equality, `store.remove_entry(pid, index)`; no-op if locator None or not found). Add `toggle_track_in_playlist` (already_has -> remove, else add). Change `pick_row` to a membership row: render a checkmark when `already_has`, `on_press` -> `toggle_track_in_playlist` (do NOT call `close_add_to_playlist`). Keep the inline "New playlist..." but change `confirm_new_playlist` to create + add + keep the overlay open (new row shows checked). Add an explicit "Done" button that calls `close_add_to_playlist` (keep the backdrop dismiss too).
- [ ] **Step 4: Run, verify pass; full `cargo test`; clippy --all-targets; fmt.**
- [ ] **Step 5: Locale** `playlists.done` (or reuse an existing Done/Close key if present) in 6 locales. Update any existing overlay test that assumed close-on-add.
- [ ] **Step 6: Commit** `feat(playlists): add-to-playlist dialogue toggles membership (add/remove)`.

---

## Task 2: Wire the now-playing heart to open the dialogue

**Files:**
- Modify: `app/rune/src/views/player.rs` (the heart at `now_playing_row`)
- Modify: `app/rune/src/views/add_to_playlist.rs` only if the now-playing snapshot needs a fallback (see below)
- Modify: `app/rune/locales/app.yml` (heart tooltip/label key)
- Test: inline tests (player.rs or main.rs)

**Interfaces:**
- Consumes: `crate::open_add_to_playlist(s, track_id)`, `icons::HEART`, the existing `transport_button`/mute-toggle button pattern.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn now_playing_heart_opens_the_add_to_playlist_dialogue() {
    let _g = config::use_test_config_dir();
    let rt = runtime_over_library(&["a.wav"]);
    let tid = rt.read(|st| st.library[0].id);
    rt.update(|st,_| st.now_playing_id = Some(tid));      // as if this track is playing
    click(&rt, "np-heart");                               // the heart's element key
    rt.read(|st| assert_eq!(st.add_to_playlist_for, Some(tid)));
}
#[test] fn no_heart_when_nothing_is_playing() {
    let rt = empty_runtime();
    rt.read(|_| {});                                      // render player panel
    assert!(!has_element(&rt, "np-heart"));               // empty state omits the heart
}
```

- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement.** Turn the muted heart glyph into a button keyed `"np-heart"`: `role("button")`, `focusable(true)`, `cursor(Pointer)`, hover/press tints, `label(t!("player.addToPlaylist"))`, `on_press(move |st| crate::open_add_to_playlist(st, track_id))` with `track_id = track.id`. Keep it visually a heart; it has no filled/liked state. Mirror the existing `transport_button`/mute-toggle button construction in `player.rs`. If `add_track_to_playlist` can't find the now-playing id in `s.library`, make it (Task 1's helpers) fall back to `s.track(id)` for the snapshot so the heart works for the now-playing track; otherwise it must no-op without panic.
- [ ] **Step 4: Run, verify pass; full `cargo test`; clippy --all-targets; fmt.**
- [ ] **Step 5: Locale** `player.addToPlaylist` in 6 locales.
- [ ] **Step 6: Commit** `feat(playlists): now-playing heart opens the add-to-playlist dialogue`.

---

## Task 3: e2e + locale completeness

**Files:**
- Modify: `app/rune/src/main.rs` (inline e2e), `app/rune/src/i18n_tests.rs` (scan add_to_playlist.rs/player.rs already in list; add any new keys to the completeness check)

- [ ] **Step 1: Write the failing e2e**

```rust
#[test] fn e2e_heart_opens_dialogue_and_toggles_membership() {
    let _g = config::use_test_config_dir();
    let rt = runtime_over_library(&["a.wav","b.wav"]);
    let pid = rt.update(|st,_| st.playlists.create("Mix"));
    let tid = rt.read(|st| st.library[0].id);
    rt.update(|st,_| st.now_playing_id = Some(tid));
    click(&rt, "np-heart");                               // heart opens dialogue
    rt.read(|st| assert_eq!(st.add_to_playlist_for, Some(tid)));
    toggle_row(&rt, pid);                                 // tick -> add
    assert_eq!(PlaylistStore::load().get(pid).unwrap().entries.len(), 1);
    toggle_row(&rt, pid);                                 // untick -> remove
    assert_eq!(PlaylistStore::load().get(pid).unwrap().entries.len(), 0);
}
```

- [ ] **Step 2-4:** fail -> pass (fix any integration gap minimally) -> full suite -> clippy -> fmt.
- [ ] **Step 5:** extend the playlists locale-completeness test to cover the new keys (`player.addToPlaylist`, `playlists.done`).
- [ ] **Step 6: Commit** `test(playlists): e2e heart -> dialogue -> toggle membership`.
