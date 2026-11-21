# Accessibility Pass Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every interactive control in `app/rune` keyboard-reachable and labelled with a valid role, give focus a sane reading order, keep dialogs keyboard-operable, and lock it in with an automatic regression sweep.

**Architecture:** All work is in `app/rune` tests + views. A data-driven sweep test renders each surface (an existing or new `--scene`) into a headless `Runtime`, walks `rt.elements()`, and asserts every actionable control has an accessible name + interactive role, with a documented allow-list. Then additive `.label()`/`.role()` fixes make the sweep green. Two more tests cover focus order and dialog keyboard operation.

**Tech Stack:** Rust (nightly, `app/rune` workspace), Rune GPU UI framework, headless `Runtime` introspection (`rt.elements()`, `rt.find()`), `rust-i18n` `t!()` for labels.

## Global Constraints

- Workspace: `app/rune` (its own Cargo workspace/lockfile, nightly toolchain). Run all commands from `app/rune/`.
- Tests are headless: `Runtime::new(state, view, (1100.0, 720.0), Env { system_dark: true, scale: 2.0 })` then `rt.frame(0.0)`.
- Labels are localized: use `t!("…")` keys, never bare English strings. Single-quote YAML values in `locales/*.yml`.
- No runtime behavior change beyond added a11y metadata and focus-order corrections. No new keyboard shortcuts (dialogs already close on Escape).
- Commit messages: conventional; NEVER reference Claude/Anthropic/AI/co-authors (commit-msg hook rejects it).
- Verify locally: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all`.
- `-ng main` moves fast in parallel sessions — merge latest `main` before pushing the PR.
- Scene fns are `fn(&mut State)`, module-private in `main.rs`; the `#[cfg(test)] mod tests` can call them directly. Test helpers already exist: `el_info(rt, key) -> ElementInfo`, `tab_to(rt, target, max) -> bool`, `key(rt, Key)`, `focused_key(rt) -> Option<String>`, `album_track(id, title)`, and the `view` fn.

---

### Task 1: Scenes for the three uncovered surfaces

Adds `--scene`s so the Appearance tab, the add-source menu, and the crash dialog can be rendered by the sweep. Scene fns are inert in normal use (only `--scene`/tests call them).

**Files:**
- Modify: `app/rune/src/main.rs` (scene fns near the other `scene_*` fns ~line 2104; registration in the `scenes:` array ~line 2325)

**Interfaces:**
- Produces: `fn scene_settings_appearance(&mut State)`, `fn scene_add_source_open(&mut State)`, `fn scene_crash_dialog(&mut State)` — callable from the test module; registered as `("settings-appearance", …)`, `("add-source-open", …)`, `("crash-dialog", …)`.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `main.rs` (near the a11y tests ~line 7850):

```rust
#[test]
fn the_new_a11y_scenes_render_their_surface() {
    // Appearance tab shows its category rail.
    let mut s = State::new(PlayerHandle::offline());
    s.sidebar_collapsed = false;
    scene_settings_appearance(&mut s);
    let mut rt = Runtime::new(s, view, (1100.0, 720.0), Env { system_dark: true, scale: 2.0 });
    rt.frame(0.0);
    assert!(rt.find("cat-appearance").is_some(), "appearance scene shows the settings rail");

    // Add-source menu open.
    let mut s = State::new(PlayerHandle::offline());
    s.sidebar_collapsed = false;
    scene_add_source_open(&mut s);
    let mut rt = Runtime::new(s, view, (1100.0, 720.0), Env { system_dark: true, scale: 2.0 });
    rt.frame(0.0);
    assert!(rt.find("add-source-opt-local").is_some(), "add-source menu is open");

    // Crash dialog.
    let mut s = State::new(PlayerHandle::offline());
    s.sidebar_collapsed = false;
    scene_crash_dialog(&mut s);
    let mut rt = Runtime::new(s, view, (1100.0, 720.0), Env { system_dark: true, scale: 2.0 });
    rt.frame(0.0);
    assert!(rt.find("crash-dialog").is_some(), "crash dialog is shown");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --bin soul-player-rune the_new_a11y_scenes_render_their_surface`
Expected: FAIL — `cannot find function scene_settings_appearance` (and the other two).

- [ ] **Step 3: Add the scene fns**

In `main.rs` after `fn scene_settings_about` (~line 2116):

```rust
/// Settings → Appearance (the tab the sweep audits for theme + nav toggles).
fn scene_settings_appearance(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::Appearance;
}
/// Settings → Music Data with the add-source menu open (its scrim makes the page
/// inert), for sweeping the modal's option buttons.
fn scene_add_source_open(s: &mut State) {
    s.route = Route::Settings;
    s.settings_tab = SettingsTab::MusicData;
    s.open_dropdown = Some("add-source".to_string());
}
/// The crash-report dialog shown after a crashed session.
fn scene_crash_dialog(s: &mut State) {
    s.crashed_last_session = true;
}
```

- [ ] **Step 4: Register the scenes**

In the `scenes: &[…]` array (~line 2332, after `("settings-about", scene_settings_about),`):

```rust
            ("settings-appearance", scene_settings_appearance),
            ("add-source-open", scene_add_source_open),
            ("crash-dialog", scene_crash_dialog),
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --bin soul-player-rune the_new_a11y_scenes_render_their_surface`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add app/rune/src/main.rs
git commit -m "test(a11y): scenes for the appearance tab, add-source menu, crash dialog"
```

---

### Task 2: The global sweep harness

The regression net: every actionable control across all surfaces has an accessible name + interactive role. Replaces the hand-enumerated `interactive_controls_expose_keyboard_a11y` test.

**Files:**
- Modify: `app/rune/src/main.rs` (test module — add the sweep; remove the old enumerated test)

**Interfaces:**
- Consumes: Task 1 scenes; `ElementInfo` fields `focusable`, `role`, `label`, `text`, `inert`, `opacity`.
- Produces: `const A11Y_ALLOWLIST`, `fn interactive_role`, `fn actionable`, `fn a11y_name`, and the surfaces list — reused by Tasks 4 and 5.

- [ ] **Step 1: Write the sweep test (will fail with real offenders)**

Add to the test module. Place the helpers above the test so Tasks 4/5 can reuse them:

```rust
/// Controls that are focusable/role-bearing but legitimately carry no accessible
/// name. (surface, element key, why). This allow-list is the ONLY suppression —
/// every entry must be justified. Start empty; add only with a documented reason.
const A11Y_ALLOWLIST: &[(&str, &str, &str)] = &[];

/// Roles denoting an interactive control, which therefore require an accessible name.
fn interactive_role(role: &str) -> bool {
    // `option` is the themed dropdown's item role (see `views/dropdown.rs`).
    matches!(
        role,
        "button" | "link" | "checkbox" | "slider" | "menuitem" | "tab" | "textbox" | "option"
    )
}

/// An element the user can operate right now: focusable or interactive-roled, and
/// actually present (not inert, not transparent). `elements()` includes opacity-0
/// nodes and `inert` ones, so both are filtered here.
fn actionable(e: &rune::ElementInfo) -> bool {
    (e.focusable || interactive_role(&e.role)) && !e.inert && e.opacity > 0.01
}

/// The accessible name: the explicit label, else the visible text (a track row
/// names itself by its text). Whitespace-only counts as empty.
fn a11y_name(e: &rune::ElementInfo) -> Option<String> {
    e.label
        .clone()
        .or_else(|| e.text.clone())
        .filter(|s| !s.trim().is_empty())
}

/// A fully-populated base state so controls (now-playing, track rows, transport)
/// actually render on every surface.
fn a11y_base() -> State {
    let mut s = State::new(PlayerHandle::recording());
    s.sidebar_collapsed = false;
    s.nav_hidden = Vec::new();
    s.library = vec![album_track(1, "Aria"), album_track(2, "Bolero")];
    s.now_playing_id = Some(1);
    s.playing = true;
    s
}

/// The surfaces the a11y sweep audits: each a scene fn applied over `a11y_base`.
const A11Y_SURFACES: &[(&str, fn(&mut State))] = &[
    ("home", scene_home),
    ("albums", scene_albums),
    ("album-detail", scene_album_detail),
    ("artists", scene_artists),
    ("artist-detail", scene_artist_detail),
    ("genres", scene_genres),
    ("queue", scene_queue),
    ("collapsed", scene_collapsed),
    ("mobile-content", scene_mobile_content),
    ("settings-music", scene_settings_music),
    ("settings-audio", scene_settings_audio),
    ("settings-audio-eq", scene_settings_audio_eq),
    ("settings-appearance", scene_settings_appearance),
    ("settings-about", scene_settings_about),
    ("settings-lang-open", scene_settings_lang_open),
    ("add-source-open", scene_add_source_open),
    ("organize-confirm", scene_organize_confirm),
    ("signal-path-open", scene_signal_path_open),
    ("crash-dialog", scene_crash_dialog),
];

/// Render one a11y surface headlessly.
fn a11y_surface(scene: fn(&mut State)) -> Runtime<State> {
    let mut s = a11y_base();
    scene(&mut s);
    let mut rt = Runtime::new(
        s,
        view,
        (1100.0, 720.0),
        Env { system_dark: true, scale: 2.0 },
    );
    rt.frame(0.0);
    rt
}

/// Every actionable control, on every surface, exposes an accessible name and a
/// valid interactive role. A dropped `.label()`/`.role()`/`.focusable()` fails here.
#[test]
fn all_actionable_controls_are_labelled() {
    let mut violations: Vec<String> = Vec::new();
    for (name, scene) in A11Y_SURFACES {
        let rt = a11y_surface(*scene);
        for e in rt.elements() {
            if !actionable(&e) {
                continue;
            }
            let key = e.key.as_deref().unwrap_or("<no key>");
            if A11Y_ALLOWLIST
                .iter()
                .any(|(s, k, _)| s == name && *k == key)
            {
                continue;
            }
            if !interactive_role(&e.role) {
                violations.push(format!(
                    "[{name}] {key}: focusable but role={:?} is not interactive",
                    e.role
                ));
            }
            if a11y_name(&e).is_none() {
                violations.push(format!("[{name}] {key}: role={:?} has no accessible name", e.role));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "a11y sweep found {} unlabelled/mis-roled controls:\n{}",
        violations.len(),
        violations.join("\n")
    );
}
```

- [ ] **Step 2: Remove the superseded enumerated test**

Delete the whole `fn interactive_controls_expose_keyboard_a11y()` test (and its inner `assert_a11y` fn) — the sweep subsumes its fixed key list. Keep `keyboard_reaches_and_activates_the_primary_nav` and `the_track_list_is_an_accessible_list_of_focusable_rows` (they assert behavior, not just metadata).

- [ ] **Step 3: Run the sweep to see the offender list**

Run: `cargo test --bin soul-player-rune all_actionable_controls_are_labelled -- --nocapture`
Expected: FAIL — prints a list like `[settings-audio] volume-slider: role="slider" has no accessible name`. Copy this list; it drives Task 3. (If it unexpectedly passes, the app is already fully labelled — skip Task 3.)

- [ ] **Step 4: Commit the harness (red)**

```bash
git add app/rune/src/main.rs
git commit -m "test(a11y): global sweep — every actionable control labelled with a valid role"
```

---

### Task 3: Fix the flagged controls

Make the sweep green by labelling real controls, de-focusing decorative ones, or allow-listing justified exceptions. Driven by Task 2's offender list — commit per view file.

**Files:**
- Modify: whichever `app/rune/src/views/*.rs` the sweep named (likely `player.rs`, `slider.rs`, `eq_graph.rs`, `settings.rs`, `dropdown.rs`, `signal_path.rs`, `queue.rs`)
- Modify: `app/rune/locales/*.yml` for any new `t!` label keys (add the key to every locale: en, de, es, zh, cs)

**Interfaces:**
- Consumes: the violation list from Task 2.

- [ ] **Step 1: Triage each offender with this decision table**

For every `[surface] key: …` line the sweep printed:
- **Real control, missing name** → add `.label(t!("…").to_string())` to that element (and `.role("button")` if role is empty). Example — a volume slider in `views/player.rs`:
  ```rust
  // before: el().key("volume-slider").focusable(true)… (no label)
  el().key("volume-slider")
      .focusable(true)
      .role("slider")
      .label(t!("player.volume").to_string())
      // …existing drag/visual…
  ```
  Add the key to each locale file, e.g. `locales/en.yml`:
  ```yaml
  player:
    volume: 'Volume'
  ```
  and the translated value in `de.yml`/`es.yml`/`zh.yml`/`cs.yml`.
- **Decorative element that should not be a tab stop** (e.g. a status dot, a scrim) → remove `.focusable(true)` / the interactive `.role(…)` instead of inventing a label.
- **Genuinely nameless-by-design interactive element** → add a line to `A11Y_ALLOWLIST` in `main.rs` with a real reason:
  ```rust
  const A11Y_ALLOWLIST: &[(&str, &str, &str)] = &[
      ("settings-audio", "eq-graph-canvas", "graph surface; labelled by its heading, operated via the band handles"),
  ];
  ```

- [ ] **Step 2: Re-run the sweep after each file**

Run: `cargo test --bin soul-player-rune all_actionable_controls_are_labelled -- --nocapture`
Expected: fewer violations each pass; iterate until it prints `test result: ok`.

- [ ] **Step 3: Confirm no regressions in the existing a11y + overflow tests**

Run: `cargo test --bin soul-player-rune a11y keyboard_reaches the_track_list cross_locale`
Expected: PASS (added labels must not overflow any locale).

- [ ] **Step 4: Commit (green), one commit per view file touched**

```bash
git add app/rune/src/views/player.rs app/rune/locales
git commit -m "fix(a11y): label the transport + volume controls"
# …repeat per file…
```

---

### Task 4: Focus order follows reading order

Assert Tab visits controls in a sane top-to-bottom / left-to-right order on the main view and a settings tab.

**Files:**
- Modify: `app/rune/src/main.rs` (test module)

**Interfaces:**
- Consumes: `a11y_surface`, `focused_key`, `key`, `el_info` helpers.

- [ ] **Step 1: Write the failing test**

```rust
/// Tabbing through a surface visits controls in reading order (each next control
/// sits at/below the previous, or to its right within the same row) — so focus
/// never jumps jarringly around the screen.
#[test]
fn focus_order_follows_reading_order() {
    const ROW_TOL: f32 = 36.0; // ~one nav row; same-row jitter allowed.
    for (name, scene) in [("home", scene_home as fn(&mut State)), ("settings-audio", scene_settings_audio)] {
        let mut rt = a11y_surface(scene);
        let mut prev: Option<[f32; 4]> = None;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..60 {
            key(&mut rt, Key::Tab);
            let Some(k) = focused_key(&rt) else { continue };
            if !seen.insert(k.clone()) {
                break; // wrapped around the focus ring
            }
            let r = el_info(&rt, &k).rect;
            if let Some(p) = prev {
                let below = r[1] >= p[1] - ROW_TOL;
                let same_row_right = (r[1] - p[1]).abs() <= ROW_TOL && r[0] >= p[0] - ROW_TOL;
                assert!(
                    below || same_row_right,
                    "[{name}] focus jumped out of reading order to {k} at {r:?} (prev {p:?})"
                );
            }
            prev = Some(r);
        }
        assert!(prev.is_some(), "[{name}] Tab reached at least one control");
    }
}
```

- [ ] **Step 2: Run to verify it fails or passes**

Run: `cargo test --bin soul-player-rune focus_order_follows_reading_order -- --nocapture`
Expected: PASS if order is already sane; FAIL printing the offending jump if a control is wired out of order.

- [ ] **Step 3: If it fails, fix focus order**

Reorder the offending control in its view so its position in the element tree matches its visual position (Rune focus order follows tree order). Re-run until PASS. If a non-reading order is intentional, relax the assertion for that surface with an inline comment explaining why.

- [ ] **Step 4: Commit**

```bash
git add app/rune/src/main.rs app/rune/src/views
git commit -m "test(a11y): focus order follows reading order on the main + audio surfaces"
```

---

### Task 5: Dialogs are keyboard-operable

Each overlay closes on Escape; true modals keep the background inert; the dropdown's items are focusable + labelled.

**Files:**
- Modify: `app/rune/src/main.rs` (test module)

**Interfaces:**
- Consumes: `a11y_surface`, `key`, `actionable`, `el_info`, `focused_key`.

- [ ] **Step 1: Write the failing test**

```rust
/// Overlays are operable from the keyboard: Escape dismisses them, and while a
/// modal is open the page behind it is inert (so Tab can't leak behind it).
#[test]
fn dialogs_are_keyboard_operable() {
    // Modals: background inert + Escape closes.
    for (name, scene, dialog_key) in [
        ("organize-confirm", scene_organize_confirm as fn(&mut State), "organize-confirm"),
        ("crash-dialog", scene_crash_dialog, "crash-dialog"),
        ("add-source-open", scene_add_source_open, "add-source-opt-local"),
    ] {
        let mut rt = a11y_surface(scene);
        assert!(rt.find(dialog_key).is_some(), "[{name}] dialog is open");
        // A background control is not actionable while the modal is up.
        let bg_leaks = rt
            .elements()
            .into_iter()
            .any(|e| e.key.as_deref() == Some("play-pause") && actionable(&e));
        assert!(!bg_leaks, "[{name}] the background stays inert behind the modal");
        key(&mut rt, Key::Escape);
        assert!(rt.find(dialog_key).is_none(), "[{name}] Escape closes the dialog");
    }

    // The language dropdown (non-modal, keyed "lang-picker"): its selectable
    // options are focusable + labelled, and Escape closes it. Option rows are
    // `lang-picker-opt-{id}` with role "option"; the optional `-opt-soon` row is
    // deliberately non-focusable, so only assert focusability where expected.
    let mut rt = a11y_surface(scene_settings_lang_open);
    let items: Vec<_> = rt
        .elements()
        .into_iter()
        .filter(|e| {
            e.role == "option"
                && e.key.as_deref().is_some_and(|k| k.starts_with("lang-picker-opt-"))
        })
        .collect();
    assert!(
        items.iter().any(|e| e.focusable),
        "the language dropdown has focusable options"
    );
    for e in &items {
        assert!(a11y_name(e).is_some(), "{:?} is labelled", e.key);
    }
    key(&mut rt, Key::Escape);
    assert!(rt.state.open_dropdown.is_none(), "Escape closes the dropdown");
}
```

- [ ] **Step 2: Run to verify it fails or passes**

Run: `cargo test --bin soul-player-rune dialogs_are_keyboard_operable -- --nocapture`
Expected: PASS if dialogs already behave; FAIL naming the dialog that leaks focus / doesn't close / has unlabelled items.

- [ ] **Step 3: If it fails, fix the dialog**

- Background leaks → ensure the overlay uses `El::modal(true, …)` (see `crash_overlay`) or its scrim marks the page inert.
- Escape doesn't close → wire the dismiss handler to Escape as the other overlays do.
- Unlabelled dropdown items → add `.label(...)`/`.focusable(true)` to the option rows.
Re-run until PASS.

- [ ] **Step 4: Commit**

```bash
git add app/rune/src/main.rs app/rune/src/views
git commit -m "test(a11y): dialogs close on Escape and keep the background inert"
```

---

### Task 6: Full verification

Confirm the whole workspace is green and formatted before the PR.

**Files:** none (verification only).

- [ ] **Step 1: Format + lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: no diff from fmt; clippy exits 0.

- [ ] **Step 2: Full test suite**

Run: `cargo test --bin soul-player-rune`
Expected: `test result: ok` — all tests pass (sweep, focus order, dialogs, and the pre-existing suite).

- [ ] **Step 3: Merge latest main, then it's ready for PR**

```bash
git fetch origin main
git merge origin/main   # resolve any conflicts; re-run the suite if main moved
```
Expected: clean or trivially-resolved merge; suite still green. The branch is now ready to push + open a PR (handled by the finishing step).
