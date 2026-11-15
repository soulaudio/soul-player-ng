# Custom Title Bar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the native window title bar with an app-drawn custom bar that keeps native window behaviour — macOS native traffic lights repositioned top-left, Windows `_ [] X` caption buttons top-right (both via Rune's `App::titlebar`), Linux native frame.

**Architecture:** Rune's `App::titlebar(Titlebar)` custom-frame mode does the OS work (native controls + resize + Aero Snap + snap-layouts). The app adds one call in `main.rs`, a small `views::titlebar` component that draws the bar content and pads clear of `cx.chrome_insets()`, and wraps the shell in a `col` so the bar sits on top. No `borderless`, no app-drawn window buttons, no `tx` control wiring (the run loop auto-handles `window_drag`).

**Tech Stack:** Rust (edition 2024, nightly toolchain in `app/rune`), the `rune` GPU UI framework, headless `Runtime` tests.

## Global Constraints

- All work is in `app/rune` — its own cargo workspace on a **nightly** toolchain. Run everything from inside `app/rune/` (`cd app/rune`): `rustup` reads the toolchain from the CWD, not `--manifest-path`.
- Tests are headless `Runtime` tests over the real engine (`Runtime::new(state, view, (w, h), env)` + `rt.frame(0.0)` + `rt.presented(key)` / `rt.find`). No GPU needed.
- `clippy --all-targets -- -D warnings` must pass; `cargo fmt` clean.
- Commit messages: **Conventional Commits, no AI/attribution trailers** (a `commit-msg` hook rejects them).
- Title bar height constant: `TITLEBAR_H = 40.0` points.
- No `unsafe` (workspace lint `unsafe_code = "forbid"`).

---

### Task 1: Scaffold `views::titlebar` and mount it atop the shell

**Files:**
- Create: `app/rune/src/views/titlebar.rs`
- Modify: `app/rune/src/views/mod.rs` (add module declaration)
- Modify: `app/rune/src/main.rs` (wrap the shell in a `col` with the bar; add test)

**Interfaces:**
- Produces: `pub(crate) const HEIGHT: f32` (= 40.0); `pub(crate) const TRAFFIC_LIGHTS: (f32, f32)`; `pub(crate) fn bar(s: &crate::State, p: &crate::theme::Palette, cx: &mut rune::Cx<crate::State>) -> rune::El` — a full-width strip keyed `"titlebar"` containing an inner `"titlebar-content"` area and a `"titlebar-title"` wordmark.

- [ ] **Step 1: Write the failing test** (in `app/rune/src/main.rs`, inside `mod tests`, next to the other view tests)

```rust
    #[test]
    fn titlebar_renders_at_the_top_full_width() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = populated_state();
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (bar, _) = rt
            .presented("titlebar")
            .expect("the custom title bar renders");
        assert!(bar[1].abs() < 1.0, "bar sits at the top, got y={}", bar[1]);
        assert!(
            (bar[2] - 1100.0).abs() < 1.0,
            "bar spans the window width, got w={}",
            bar[2]
        );
        assert!(
            (bar[3] - views::titlebar::HEIGHT).abs() < 1.0,
            "bar is HEIGHT tall, got h={}",
            bar[3]
        );
    }
```

- [ ] **Step 2: Run the test — verify it fails**

Run: `cd app/rune && cargo test -q titlebar_renders_at_the_top_full_width 2>&1 | tail -15`
Expected: FAIL to compile (`views::titlebar` does not exist) or a panic that `"titlebar"` is not presented.

- [ ] **Step 3: Create the component** — `app/rune/src/views/titlebar.rs`

```rust
//! The app-drawn window title bar. Rune's `App::titlebar` (set in `main.rs`) keeps
//! the native window controls — macOS traffic lights at the leading edge, Windows
//! `_ [] X` caption buttons at the trailing edge, Linux's native frame — and this
//! draws the bar they sit in. `cx.chrome_insets()` says how much space those
//! controls take at each end; the content keeps clear of both. `.window_drag()`
//! makes the strip move the window (double-click zooms); the run loop handles it.
#![forbid(unsafe_code)]

use rune::{Cx, El, row, text};

use crate::theme::Palette;
use crate::ui::{space, type_};
use crate::State;

/// Title-bar height, in points. Tall enough to centre the macOS traffic lights
/// and host the Windows caption buttons.
pub(crate) const HEIGHT: f32 = 40.0;

/// Where the macOS close button's top-left sits inside the bar (Electron's
/// `trafficLightPosition`): standard left padding, vertically centred for the
/// ~12 pt lights. `main.rs` passes this to `Titlebar::traffic_lights`.
pub(crate) const TRAFFIC_LIGHTS: (f32, f32) = (19.0, (HEIGHT - 12.0) / 2.0);

/// The window title bar: a full-width strip, `HEIGHT` tall. Minimal for now (a
/// wordmark); the open centre/trailing area is where custom header components go
/// later. Task 2 makes the gutters clear the native controls via `chrome_insets`.
pub(crate) fn bar(_s: &State, p: &Palette, _cx: &mut Cx<State>) -> El {
    row()
        .key("titlebar")
        .w_full()
        .h(HEIGHT)
        .fill(p.bg)
        .items_center()
        .px(space::MD)
        .window_drag()
        .child(
            row()
                .key("titlebar-content")
                .grow(1.0)
                .min_w(0.0)
                .h_full()
                .items_center()
                .child(
                    text("Soul Audio".to_string())
                        .key("titlebar-title")
                        .font(type_::LABEL)
                        .color(p.text_secondary),
                ),
        )
}
```

(The `_cx` parameter is kept now so Task 2 can read `cx.chrome_insets()` without a signature change.)

- [ ] **Step 4: Register the module** — add to `app/rune/src/views/mod.rs` (alphabetical with the other `pub mod` lines)

```rust
pub mod titlebar;
```

- [ ] **Step 5: Mount the bar atop the shell** — in `app/rune/src/main.rs`, in `fn view`, replace the shell construction (the `let shell = if mobile { ... } else { ... };` block) so the `"app-root"` key moves to an outer `col` and the old shell becomes an inner `"shell"` that grows below the bar:

```rust
    // Inner shell: the old desktop/mobile layout, now under the title bar.
    let shell_inner = if mobile {
        let root = col().key("shell").w_full().grow(1.0).min_h(0.0).fill(p.bg);
        if s.mobile_show_content {
            root.child(views::sidebar::mobile_top_bar(mobile_title(s), &p))
                .child(content_pane(s, &p, cx, scale))
        } else {
            root.child(views::sidebar::sidebar(s, &p, cx, scale, true))
        }
    } else {
        row()
            .key("shell")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .fill(p.bg)
            .child(if s.sidebar_collapsed {
                col().key("sidebar-slot").w(0.0).h_full()
            } else {
                views::sidebar::sidebar(s, &p, cx, scale, false)
            })
            .child(views::sidebar::resize_handle(s.sidebar_collapsed, &p))
            .child(content_pane(s, &p, cx, scale))
    };

    // The window shell: the custom title bar over the content. `app-root` keeps
    // the key `window_w`/mobile detection measure (a col reports its width too).
    let shell = col()
        .key("app-root")
        .w_full()
        .h_full()
        .fill(p.bg)
        .child(views::titlebar::bar(s, &p, cx))
        .child(shell_inner);
```

(The existing `let window_w = cx.layout_size("app-root")...` and `let mobile = ...` lines above stay unchanged; the `overlays` block below stays unchanged — it already wraps `shell`.)

- [ ] **Step 6: Run the test — verify it passes**

Run: `cd app/rune && cargo test -q titlebar_renders_at_the_top_full_width 2>&1 | tail -8`
Expected: PASS (`1 passed`).

- [ ] **Step 7: Commit**

```bash
git add app/rune/src/views/titlebar.rs app/rune/src/views/mod.rs app/rune/src/main.rs
git commit -m "feat(ui): custom title-bar strip mounted atop the shell"
```

---

### Task 2: Keep bar content clear of the native controls (chrome insets)

**Files:**
- Modify: `app/rune/src/main.rs` (add test)
- Modify: `app/rune/src/views/titlebar.rs` (make the gutters chrome-aware + theme the Windows buttons)

**Interfaces:**
- Consumes: `views::titlebar::bar` and its `"titlebar-content"` inner area from Task 1.
- Produces: `bar` now reads `cx.chrome_insets()` and calls `cx.caption_color` / `cx.titlebar_height`.

- [ ] **Step 1: Write the failing test** (in `app/rune/src/main.rs`, `mod tests`)

```rust
    #[test]
    fn titlebar_content_stays_within_the_chrome_insets() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = populated_state();
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        // Simulate macOS traffic lights (leading) + Windows caption buttons
        // (trailing), as a native window reports them.
        rt.set_chrome_insets(rune::ChromeInsets::new(80.0, 138.0, views::titlebar::HEIGHT));
        rt.frame(0.0);
        let (content, _) = rt
            .presented("titlebar-content")
            .expect("the bar content area renders");
        assert!(
            content[0] >= 80.0 - 0.5,
            "content clears the leading (macOS) inset, x={}",
            content[0]
        );
        assert!(
            content[0] + content[2] <= 1100.0 - 138.0 + 0.5,
            "content clears the trailing (Windows) inset, right={}",
            content[0] + content[2]
        );
    }
```

- [ ] **Step 2: Run the test — verify it fails**

Run: `cd app/rune && cargo test -q titlebar_content_stays_within_the_chrome_insets 2>&1 | tail -12`
Expected: FAIL — with the symmetric `.px(space::MD)` gutter from Task 1, the content starts at `space::MD` (12), not clearing the simulated `leading = 80` inset (and runs past `window_w - trailing`).

- [ ] **Step 3: Make the gutters chrome-aware** — in `app/rune/src/views/titlebar.rs`, update `bar` to read the insets and theme the Windows buttons. Replace the `.items_center().px(space::MD)` line and the `_cx` parameter:

```rust
pub(crate) fn bar(_s: &State, p: &Palette, cx: &mut Cx<State>) -> El {
    let chrome = cx.chrome_insets();
    // Tint the Windows caption-button symbols to the theme and keep the bar's
    // height in sync with the app's zoom.
    cx.caption_color(p.fg);
    cx.titlebar_height(HEIGHT);

    row()
        .key("titlebar")
        .w_full()
        .h(HEIGHT)
        .fill(p.bg)
        .items_center()
        // Clear the native controls: macOS lights (leading), Windows buttons
        // (trailing). `max(space::MD)` keeps a sane gutter where an inset is 0.
        .pl(chrome.leading.max(space::MD))
        .pr(chrome.trailing.max(space::MD))
        .window_drag()
        .child(
            row()
                .key("titlebar-content")
                .grow(1.0)
                .min_w(0.0)
                .h_full()
                .items_center()
                .child(
                    text("Soul Audio".to_string())
                        .key("titlebar-title")
                        .font(type_::LABEL)
                        .color(p.text_secondary),
                ),
        )
}
```

- [ ] **Step 4: Run the test — verify it passes**

Run: `cd app/rune && cargo test -q titlebar_content_stays_within_the_chrome_insets 2>&1 | tail -8`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add app/rune/src/views/titlebar.rs app/rune/src/main.rs
git commit -m "feat(ui): title-bar gutters clear the macOS/Windows native controls"
```

---

### Task 3: Enable the native controls — `App::titlebar` in `main.rs`

**Files:**
- Modify: `app/rune/src/main.rs` (the `App::new(spec)` builder chain in `main`)

**Interfaces:**
- Consumes: `views::titlebar::{HEIGHT, TRAFFIC_LIGHTS}` from Task 1.

This is the window-level wiring that makes the OS draw native traffic lights (macOS) / caption buttons (Windows). It is not unit-testable (App construction, not the view), so it is verified by a clean build plus an on-device launch.

- [ ] **Step 1: Add the titlebar to the App builder** — in `app/rune/src/main.rs`, find `let app = App::new(spec)` and add a `.titlebar(...)` call to the chain (import `Titlebar` alongside the existing `rune::…` imports at the top of `main.rs`):

```rust
    let app = App::new(spec)
        .titlebar(
            rune::Titlebar::new(views::titlebar::HEIGHT)
                .traffic_lights(views::titlebar::TRAFFIC_LIGHTS.0, views::titlebar::TRAFFIC_LIGHTS.1),
        )
        // ... existing chain continues (hotkeys, etc.) ...
```

(Add `Titlebar` to the top-of-file `use rune::{...}` import list if not already present.)

- [ ] **Step 2: Build — verify it compiles**

Run: `cd app/rune && cargo build 2>&1 | tail -5`
Expected: `Finished` with no errors.

- [ ] **Step 3: Launch and verify on-device** (manual; do on whatever OS you're on)

Run: `cd app/rune && ./target/debug/soul-player-rune.exe` (Windows) or `./target/debug/soul-player-rune` (macOS/Linux)
Expected:
- The native title bar text strip is gone; the app's bar fills the top.
- **macOS:** the three traffic lights sit at the top-left inside the bar (hover shows ×/−/+; green zooms); dragging the bar moves the window; double-click zooms.
- **Windows:** `_ [] X` at the top-right, tinted to the theme; hovering maximize shows the Win11 snap-layouts flyout; edges resize; Aero Snap works; dragging the bar moves the window.
- **Linux:** native frame stays (expected).

- [ ] **Step 4: Commit**

```bash
git add app/rune/src/main.rs
git commit -m "feat(ui): enable App::titlebar so the OS draws native window controls"
```

---

### Task 4: Regression — the title bar doesn't break the shell / overflow net

**Files:**
- Modify: `app/rune/src/main.rs` (add test)

**Interfaces:**
- Consumes: the wrapped shell from Task 1.

- [ ] **Step 1: Write the failing test** (in `app/rune/src/main.rs`, `mod tests`) — the title bar must not break the `"app-root"` width measurement that drives mobile detection, and the content must still sit below it.

```rust
    #[test]
    fn titlebar_sits_above_the_shell_without_breaking_app_root() {
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let s = populated_state();
        apply_locale("en");
        let mut rt = Runtime::new(s, view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let (root, _) = rt.presented("app-root").expect("app-root renders");
        let (bar, _) = rt.presented("titlebar").expect("the bar renders");
        let (shell, _) = rt.presented("shell").expect("the shell renders");
        // app-root still spans the window width (mobile detection reads this).
        assert!((root[2] - 1100.0).abs() < 1.0, "app-root is window-width, got {}", root[2]);
        // The shell sits directly below the bar (no overlap, no gap).
        assert!(
            (shell[1] - (bar[1] + bar[3])).abs() < 1.0,
            "shell starts at the bar's bottom: bar={bar:?} shell={shell:?}"
        );
    }
```

- [ ] **Step 2: Run the test — verify it passes**

Run: `cd app/rune && cargo test -q titlebar_sits_above_the_shell_without_breaking_app_root 2>&1 | tail -8`
Expected: PASS. (If `"shell"` isn't found, confirm Task 1 Step 5 keyed the inner layout `"shell"` in both the mobile and desktop branches.)

- [ ] **Step 3: Run the cross-locale overflow net + full suite — verify no regressions**

Run: `cd app/rune && cargo test -q contains_itself 2>&1 | tail -4 && cargo test -q 2>&1 | grep -E 'test result:' | head -2`
Expected: `contains_itself` tests PASS; the full suite shows `... passed; 0 failed` (ignore the known pre-existing `removal_notice_renders_in_the_sources_page` locale-race flake if it appears — run it alone to confirm it passes in isolation).

- [ ] **Step 4: fmt + clippy**

Run: `cd app/rune && cargo fmt && cargo clippy -q --all-targets 2>&1 | grep -E 'warning:|error' | head`
Expected: no warnings/errors from the new code.

- [ ] **Step 5: Commit**

```bash
git add app/rune/src/main.rs
git commit -m "test(ui): title bar sits above the shell without breaking app-root"
```

---

## Self-Review

**Spec coverage:**
- macOS native traffic lights repositioned → Task 3 (`App::titlebar` + `traffic_lights`).
- Windows `_ [] X` top-right with native behaviour → Task 3.
- Linux native frame → Task 3 (no-op), verified by the on-device note.
- App-drawn bar + room for custom components → Task 1 (`titlebar::bar`, open content area).
- Content clears the native controls → Task 2.
- Theming (`caption_color`, palette) → Task 1 implementation.
- Shell integration without breaking `window_w`/mobile → Task 1 + Task 4.
- Testing strategy (headless render, inset-clearing, regression, overflow net) → Tasks 1, 2, 4.

**Placeholder scan:** none — every step has exact code/commands.

**Type consistency:** `HEIGHT`/`TRAFFIC_LIGHTS` defined in Task 1, consumed in Task 3; `bar` signature and the `"titlebar"`/`"titlebar-content"`/`"shell"`/`"app-root"` keys are consistent across Tasks 1, 2, 4; `rune::ChromeInsets::new(leading, trailing, height)` and `rt.set_chrome_insets` match the framework.

**Known deferrals (non-blocking):** a 1 px bottom hairline on the bar (visual polish) and the future custom header components are intentionally out of scope; the bar ships as a minimal, app-owned strip.
