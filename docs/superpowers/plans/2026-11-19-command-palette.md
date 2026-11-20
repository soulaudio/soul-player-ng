# Command Palette Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A Ctrl/Cmd-K command palette overlay to run any app action or jump anywhere from the keyboard.

**Architecture:** App-only. A pure command registry + filter/selection logic, a themed `El::modal` overlay mirroring `views::add_to_playlist`, opened from `handle_key` via `ev.is_command('k')`, with `on_input_key` driving Up/Down/Enter over a `State.command_palette_selected` index.

**Tech Stack:** Rust; `app/rune` (nightly from CWD); Rune immediate-mode UI; rust-i18n.

## Global Constraints

- Modify ONLY `app/rune` (`main.rs`, `views/command_palette.rs` new, `locales/app.yml`, `i18n_tests.rs`, tests). NO backend/host/core.
- NO em-dashes (grep `\x{2014}` added lines). NO attribution in commits.
- Every command label localized in 6 locales (en/de/es/zh/cs/ja), via a `match id -> t!("command.<id>")` (dynamic t! keys are disallowed).
- Overlay is the themed `El::modal` (Escape + outside-click dismiss free); NOT a native picker.
- Keyboard-nav / render tests use the full-app harness (`Runtime::new(s, view, (1100,720), env)` + `click_key`/key events); the panel-only harness does not route events.
- TDD; `cargo clippy -p soul-player-rune --all-targets -- -D warnings`; slow compile, patient timeouts, no background+poll.

## Reference (from recon, exact)
- `handle_key(s, ev, tx) -> bool` at `main.rs:~2854`, wired `on_key: Some(handle_key)` (`:3337`). `true` consumes. `KeyEvent::is_command(c)` = Cmd/Ctrl chord. `Key` enum is `#[non_exhaustive]` (needs `_` arm).
- Overlay mount: `overlays.extend(views::add_to_playlist::overlay(s, &p));` at `main.rs:~2507` inside the `"overlay-root"` block - add the palette overlay beside it (before `crash_overlay`, which stays top).
- Overlay template: `views/add_to_playlist.rs` - scrim `col().absolute(0,0).w_full().h_full().center().fill(Color::BLACK.alpha(0.45))` + inner `col().key(..).w(..).fill(p.card).radius(radius::LG).border(1.0,p.border).padding(space::LG).modal(true, crate::close_command_palette)`.
- Text input: `text_input(&s.command_palette_query, type_::ITEM).key("command-palette-input").placeholder(..).single_line().on_edit(|st| &mut st.command_palette_query).on_input_key(|st, ev, _tx| { ... })`.
- `TextEdit { text, focused: bool, .. }` - set `.focused = true` on open to autofocus.
- Invoke calls (each `fn(&mut State)`): route set mirrors `sidebar.rs:174` (`s.nav_history.clear(); s.genre_search = TextEdit::new(""); s.playlist_new_buf = TextEdit::new(""); s.route = route; s.mobile_show_content = true;`); `set_settings_tab(s, tab)`; `s.host.send(Command::{TogglePlay,Next,Prev,Stop,SetShuffle(!s.shuffle),SetRepeat(next),ClearQueue})`; `toggle_mute(s)`; `set_theme_pref(s, ThemePref::X)`; `set_zoom(s, stepped_zoom(s.ui_zoom, +/-1))` / `set_zoom(s, 1.0)`; `open_add_to_playlist(s, s.now_playing_id?)`.

---

## Task 1: State, registry, filter logic, i18n labels, Cmd/Ctrl-K trigger

**Files:** Create `app/rune/src/views/command_palette.rs`; Modify `app/rune/src/main.rs` (State fields + open/close helpers + `mod`/`handle_key` branch), `app/rune/locales/app.yml`, `app/rune/src/i18n_tests.rs`. Test: inline in `command_palette.rs` + `main.rs`.

**Interfaces (produces):**
```rust
// command_palette.rs
pub(crate) struct PaletteCommand { pub id: &'static str, pub action: fn(&mut State) }
pub(crate) fn commands(s: &State) -> Vec<PaletteCommand>;  // precondition-filtered (e.g. add-current only when playing)
pub(crate) fn command_label(id: &str) -> String;           // match id -> t!("command.<id>")
pub(crate) fn filtered(cmds: &[PaletteCommand], query: &str) -> Vec<usize>;  // indices whose label substring-matches (ci); empty query = all
// main.rs
command_palette_open: bool, command_palette_query: rune::TextEdit, command_palette_selected: usize
pub(crate) fn open_command_palette(s: &mut State);   // open=true, query=TextEdit::new("") with focused=true, selected=0
pub(crate) fn close_command_palette(s: &mut State);  // open=false, query reset, selected=0
```

- [ ] **Step 1: Failing tests**
```rust
// command_palette.rs
#[test] fn filtered_substring_matches_labels_case_insensitively() {
    apply_locale("en");
    let cmds = commands(&State::new(PlayerHandle::offline()));
    let all = filtered(&cmds, "");
    assert_eq!(all.len(), cmds.len(), "empty query shows all");
    let play = filtered(&cmds, "PLAY");   // "Play/Pause", "Playlists", "New playlist"...
    assert!(!play.is_empty() && play.iter().all(|&i| command_label(cmds[i].id).to_lowercase().contains("play")));
    assert!(filtered(&cmds, "zzzxxx").is_empty());
}
#[test] fn every_command_has_a_nonempty_localized_label() {
    apply_locale("en");
    for c in commands(&State::new(PlayerHandle::offline())) {
        let l = command_label(c.id);
        assert!(!l.is_empty() && l != format!("command.{}", c.id), "label for {}", c.id);
    }
}
// main.rs
#[test] fn ctrl_k_opens_the_command_palette() {
    let mut s = State::new(PlayerHandle::offline());
    let mut tx = /* a test Tx if constructible, else drive via Runtime */;
    // prefer: assert open_command_palette sets the flag + handle_key routes is_command('k')
    open_command_palette(&mut s);
    assert!(s.command_palette_open && s.command_palette_query.focused);
    close_command_palette(&mut s);
    assert!(!s.command_palette_open);
}
```
(If `handle_key` can't be unit-tested without a `Tx`, assert the open/close helpers here and cover the Cmd-K keystroke in Task 3's full-app e2e via `rt.key(...)`.)

- [ ] **Step 2-4:** fail -> implement -> pass. In `handle_key`, add `if ev.is_command('k') { open_command_palette(s); return true; }` near the top (after the repeat guard, before the shortcut match). Add `command.*` keys for every command id in all 6 locales; add `command_palette.rs` to the `i18n_tests.rs` scan list.
- [ ] **Step 5: Commit** `feat(palette): command registry, filter, Cmd/Ctrl-K trigger`.

---

## Task 2: The overlay (render + keyboard nav + click) + mount

**Files:** Modify `app/rune/src/views/command_palette.rs` (the `overlay` fn), `app/rune/src/main.rs` (mount at `overlays.extend`). Test: inline full-app render/keyboard tests.

**Interfaces:** `pub fn overlay(s: &State, p: &Palette) -> Option<El>` (None when closed).

- [ ] **Step 1: Failing tests** (full-app harness)
```rust
#[test] fn palette_opens_filters_and_enter_runs_the_selected_command() {
    let env = Env { system_dark: true, scale: 2.0 };
    let mut s = populated_state(); apply_locale("en");
    let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
    rt.frame(0.0);
    rt.key_chord_command('k');                 // or the real key path that triggers is_command('k'); grep existing key tests
    rt.frame(0.0);
    assert!(rt.state.command_palette_open);
    assert!(rt.find("command-palette-input").is_some());
    rt.ime(Ime::Commit("albums".into())); rt.frame(0.0);   // filter to the Albums nav command
    rt.key(Key::Enter); rt.frame(0.0);
    assert!(matches!(rt.state.route, Route::Albums));
    assert!(!rt.state.command_palette_open, "running a command closes the palette");
}
#[test] fn palette_closed_renders_no_overlay() {
    let s = State::new(PlayerHandle::offline());
    assert!(views::command_palette::overlay(&s, &crate::theme::palette(true)).is_none());
}
```
(Grep existing tests for the exact key-injection API - `rt.key`, `rt.ime`, how a Cmd/Ctrl chord is synthesized. If there is no helper to synthesize `is_command('k')`, open the palette via `rt.update(|st,_| open_command_palette(st))` and still test the filter+Enter path through the rendered overlay.)

- [ ] **Step 2-4:** fail -> implement -> pass. The overlay: scrim + inner `modal(true, close_command_palette)`; a `text_input` keyed `command-palette-input` bound to `command_palette_query` with `.on_input_key(|st, ev, _tx| ...)` handling `Key::Up`/`Key::Down` (move `command_palette_selected`, clamped to `filtered` len; return true) and `Key::Enter` (take the selected command, `close_command_palette(st)` then run its `action`; return true), else `false`; a scrolled `col().role("list").reveal_selected(true)` of rows (one per filtered command, keyed `command-row-{id}`), the selected row highlighted (fill/accent), each `.on_press(|st| { let action = ...; close_command_palette(st); action(st); })`. Re-clamp `selected` to 0 on query change (in the `on_edit` path or when `filtered` shrinks). Run full suite + clippy --all-targets + fmt.
- [ ] **Step 5: Commit** `feat(palette): overlay with keyboard nav + click`.

---

## Task 3: e2e + locale completeness

**Files:** Modify `app/rune/src/main.rs` (e2e), `app/rune/src/i18n_tests.rs`.

- [ ] **Step 1: e2e** (full app): open palette (Cmd/Ctrl-K or `open_command_palette`), type a query, Down to move selection, Enter runs the selected command (assert effect), palette closes; separately, open -> Escape (or `close_command_palette`) closes and renders no overlay. Plus a click-to-run path.
- [ ] **Step 2: Locale completeness:** extend `i18n_tests.rs` so every `command.*` key is present in all 6 locales (if the completeness test globs namespaces it may already cover them; verify + add explicit assertion if needed).
- [ ] **Step 3-5:** fail -> pass -> full suite -> clippy --all-targets -> fmt -> commit `test(palette): e2e open -> filter -> run + locale completeness`.
