# Accessibility pass — design

Issue: #104 (Accessibility pass — focus order, labels, a11y test harness)
Date: 2026-11-16
Scope: `app/rune` (the Rune desktop head). Headless tests only; no user-visible behaviour change beyond controls gaining accessible metadata and any focus-order corrections.

## Goal

Make every interactive control in the app keyboard-reachable and labelled, give focus a sane reading order, and keep dialogs keyboard-operable — then lock it in with an **automatic regression net** so a dropped `.label()`/`.role()`/`.focusable()` fails the build instead of silently shipping.

Today's harness (`interactive_controls_expose_keyboard_a11y`, `keyboard_reaches_and_activates_the_primary_nav`, `the_track_list_is_an_accessible_list_of_focusable_rows`) proves the pattern but enumerates a **fixed list** of ~20 keys, so a new control added without a11y metadata is not caught, and focus *order* is never asserted. This pass replaces the enumerated list with a sweep and adds order + dialog coverage.

## Non-goals

- Screen-reader-quality label *wording* review (we assert names are present and non-empty, not that they read perfectly).
- Colour-contrast / visual a11y (separate concern; theme already dual-mode).
- Changing interaction models (no new keyboard shortcuts beyond what dialogs already imply — Escape to close).

## Design

### 1. The sweep — `all_actionable_controls_are_labelled`

A single data-driven test over a list of **surfaces**. Each surface is an existing `--scene` applied to a fresh `Runtime::new(state, view, (1100,720), Env{dark,2.0})`, advanced with `rt.frame(0.0)`.

For each surface it collects every **actionable** element from `rt.elements()`:

```
actionable(e) = (e.focusable || INTERACTIVE_ROLES.contains(e.role))
             && !e.inert
             && e.opacity > 0.01
```

and asserts, for each:

- **accessible name present**: `name(e)` is non-empty, where `name(e)` = `e.label` if set, else `e.text` (text-labelled controls such as a track row carry their name as text). An empty name fails.
- **valid interactive role**: `e.role` is in `INTERACTIVE_ROLES` (not `""`, not `"none"`). A focusable element with a structural/absent role fails (it should either carry an interactive role or not be focusable).

`INTERACTIVE_ROLES = {button, link, checkbox, slider, menuitem, tab, textbox}` (final set confirmed against what the views actually emit during implementation — the test prints the offending `(scene, key, role)` on failure so the set is easy to right-size).

**Allow-list.** `const A11Y_ALLOWLIST: &[(&str, &str, &str)]` of `(scene, key, reason)`. The sweep skips `(scene, key)` pairs listed here. Every entry must carry a human reason (e.g. a decorative focus target that is intentionally unnamed). Strict by default: the allow-list is the *only* way to suppress, and each suppression is reviewable in the diff.

Failure message names the surface, key, role, and which check failed, so fixes are mechanical.

### 2. Surfaces

Reuse existing scenes; add two for the uncovered modals.

- **Main pages:** `home`, `albums`, `album-detail`, `artists`, `artist-detail`, `genres`, (default/tracks), `queue`, `collapsed`, `mobile-content`.
- **Settings tabs:** `settings-music`, `settings-audio`, `settings-audio-eq`, `settings-about`, and Appearance (via a plain `appearance` scene — add one; today only the German `settings-appearance-de` exists).
- **Overlays:** `settings-lang-open` (language dropdown), `organize-confirm` (organize modal), `signal-path-open` (signal-path panel).
- **New scenes to add:** `add-source-open` (the add-source menu/modal) and `crash-dialog` (the crash-report dialog), so every overlay is swept. These are small `fn scene_*(s)` tweaks registered in the `scenes` array, matching the existing pattern.

Surfaces that need a populated library/now-playing (so controls render) seed it in the scene the same way existing scenes do.

### 3. Focus order — `focus_order_follows_reading_order`

For two representative surfaces (the main view with a library + now-playing, and a settings tab), Tab from the top through all focusables (`tab_to`-style loop, bounded), recording each focused element's `rect`. Assert the visited sequence is in **reading order within a tolerance**: for consecutive focused elements `a → b`, either `b.top >= a.top - ROW_TOL` (next is at/below), or they share a row (`|b.top - a.top| <= ROW_TOL`) and `b.left >= a.left` (left-to-right). `ROW_TOL ≈ one row height`. This catches a control wired into focus far from its visual position without hard-coding an exact sequence (robust to adding controls). If a flow legitimately needs a non-reading order, it is documented inline where the assertion is relaxed.

### 4. Dialogs are keyboard-operable — `dialogs_are_keyboard_operable`

Per overlay — add-source menu, organize modal, language dropdown — assert:

- **Escape closes it**: open the overlay (scene or trigger), `key(Escape)`, then the overlay key is gone / `open_dropdown` is `None`.
- **Background is contained**: while open, a known background control (e.g. `play-pause`) is **not actionable** — absent from `elements()` or `inert` — confirming the scrim/modal makes the page inert so Tab can't leak behind the dialog. (The dropdown is deliberately non-modal per `views::dropdown`; for it we assert instead that its items are focusable + labelled and Escape closes it.)

### 5. Fixes

Run the sweep; fix every flagged control. Anticipated (additive `.label()`, occasionally a `.role()`):

- sliders — volume, EQ band handles;
- icon-only buttons — close/dismiss, scrim, status dots acting as buttons;
- the dropdown trigger, source rows, profile switcher, signal-path toggle.

Each fix is metadata only (label/role); no logic changes. Where a focusable element is actually decorative, prefer removing `.focusable()` over inventing a label; where it is a real control, label it.

## Testing & validation

- All tests live in `app/rune` (nightly toolchain), run headless via `Runtime` + `rt.elements()` — the same introspection the cross-locale overflow net uses.
- `cargo test` green, `cargo clippy --all-targets -- -D warnings` clean, `cargo fmt`.
- The sweep itself is the acceptance check: zero un-allow-listed actionable controls without a name/role across all surfaces.

## Risks / notes

- `elements()` includes `opacity: 0` nodes and `find()` ignores opacity (see `rune-poll-driven-animation` memory) — hence the sweep filters on `opacity > 0.01 && !inert`.
- `-ng main` moves fast in parallel sessions (e.g. #186 landed loudness while this was scoped) — merge latest `main` before pushing the PR.
- Scene additions are inert in normal use (only `--scene` runs them), matching existing snapshot scenes.
