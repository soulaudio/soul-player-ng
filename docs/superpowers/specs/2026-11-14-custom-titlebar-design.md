# Custom title bar — design

**Date:** 2026-11-14
**Status:** Approved, ready for implementation plan
**Area:** `app/rune` (native Rune UI head)

## Goal

Replace the native OS window title bar with an app-drawn custom bar that behaves
to each platform's convention, and that leaves a home for future custom header
components:

- **macOS** — keep the real native traffic lights (close / minimise / zoom),
  repositioned into the custom bar at the top-left.
- **Windows** — minimise / maximise / close (`_ [] X`) at the top-right.
- **Linux** — `_ [] X` at the top-right.

"Working perfectly" is an explicit requirement: native window behaviours
(edge-resize, Aero Snap, the Windows 11 snap-layouts flyout, double-click-zoom,
full-screen, window shadow, rounded corners) must be preserved, not reimplemented.

## Key decisions (resolved during brainstorming)

1. **macOS controls stay native, repositioned** — not custom-drawn. Web research
   (Electron `titleBarStyle: hidden/hiddenInset` + `trafficLightPosition`, Tauri
   `Overlay` + `trafficLightPosition`, and multiple real-world reports) is
   unanimous that faking traffic lights loses the hover glyphs, option-click-zoom,
   full-screen tiling menu, accessibility (AX) controls, and native corners/shadow,
   and reads as "off". Rune supports repositioned-native directly.
2. **Perfect behaviour wins over pixel-custom buttons/Linux-custom-bar.** Rune's
   `App::titlebar()` custom-frame mode keeps all native window behaviour on macOS
   and Windows while letting the app draw the bar; a fully *borderless* window
   (needed to draw 100%-custom buttons, or a custom Linux bar) would sacrifice
   edge-resize, Aero Snap, and snap-layouts. We take the native-assisted path.
3. **Linux keeps its native frame.** `App::titlebar()` is a no-op on Linux (Rune
   keeps the native decorations), which is what Linux desktop users expect. A
   custom Linux bar is explicitly a non-goal here (it would require borderless +
   app-implemented resize, i.e. a Rune-level feature).

## Architecture

Rune does the OS-level work; the app does three small things.

### 1. Enable the custom title bar (window setup, `main.rs`)

Call `App::titlebar(Titlebar::new(TITLEBAR_H).traffic_lights(x, y))` on the
existing `App::new(spec)`. Called unconditionally:

- macOS → native traffic lights drawn by the OS, positioned at `(x, y)` within the
  bar; native drag / double-click-zoom / full-screen kept.
- Windows → Rune draws the `_ [] X` caption buttons as elements at the trailing
  edge *and* wires them as real `HTMINBUTTON` / `HTMAXBUTTON` / `HTCLOSE` hits, so
  Aero Snap, the Win11 snap-layouts flyout, resize edges, and the window menu all
  work. No app-drawn buttons, no `tx.minimize/close` wiring needed.
- Linux → no-op; native frame stays.

### 2. `views::titlebar` — the bar content (new module)

A full-width strip, `TITLEBAR_H` points tall, that:

- Reads `cx.chrome_insets()` (`leading` = macOS traffic lights, `trailing` =
  Windows caption buttons, both 0 where the native frame stays) and pads its
  content clear of both ends, at any width/zoom.
- Calls `cx.caption_color(p.fg)` and `cx.titlebar_height(TITLEBAR_H)` each frame so
  the Windows buttons match the theme colour and the bar height scales with the UI.
- Is a `.window_drag()` band (press = move window, double-press = zoom/minimise per
  the system setting). Interactive children (`on_press`, focusable, future
  controls) already keep their own presses; `.no_window_drag()` is available for
  any inert decorative gap that should still move the window's neighbours.
- Content is intentionally minimal for now: the themed strip (palette `bg`, a 1px
  bottom `border`) with a left-aligned "Soul Audio" wordmark placeholder after the
  leading inset. The centre/trailing area is left open for future custom
  components (the reason the user wants an app-owned bar).

The app draws **no** window-control buttons itself — all controls are native
(macOS) or Rune-drawn-and-OS-wired (Windows). This keeps the component tiny and
the behaviour perfect.

### 3. Shell integration (`view()` in `main.rs`)

`app-root` becomes a vertical stack:

```
col().key("app-root").w_full().h_full()
  .child(views::titlebar::bar(s, &p, cx))   // TITLEBAR_H tall, full width
  .child(shell_row)                          // existing [sidebar | content], grow(1.0)
```

The `col` keeps the `"app-root"` key that `window_w` / mobile detection measure
(a col reports its laid-out width the same as the row did); the existing
sidebar|content row moves under a new inner key with `grow(1.0).min_h(0.0)`. The
overlay layer (dropdowns, signal-path panel) continues to wrap the whole thing.

## Rune APIs used

- `App::titlebar(Titlebar)`, `Titlebar::new(height)`, `Titlebar::traffic_lights(x, y)`
- `Cx::chrome_insets() -> ChromeInsets { leading, trailing, height }`
- `Cx::caption_color(Color)`, `Cx::titlebar_height(f32)`
- `El::window_drag()`, `El::no_window_drag()`

No `borderless`, no custom window-control buttons, no `tx.minimize/toggle_maximized/close_window`.

## Constants

- `TITLEBAR_H: f32 = 40.0` — bar height in points. Tall enough to centre the macOS
  traffic lights and host the Windows caption buttons; tunable. Traffic-light
  origin `(x, y)` chosen to sit the lights with standard left padding, vertically
  centred (≈ `x = 20`, `y = (TITLEBAR_H - 12)/2`), refined on-device.

## Testing

Headless `Runtime` tests over the real layout/paint engine (the project's standard;
window chrome is reported to headless runs):

- The bar renders at the top, `TITLEBAR_H` tall, full window width.
- Bar content stays clear of a simulated `leading` inset (macOS case) and a
  simulated `trailing` inset (Windows case) — nothing overlaps the chrome.
- The drag band carries `window_drag` (assert via `rt.elements()` / the window-drag
  flag) and the wordmark/placeholder is inside the safe region.
- The shell still composes: `app-root` width still equals the window width (mobile
  breakpoint detection unaffected), and the existing
  `app_layout_contains_itself_in_every_locale` net still passes with the bar added.

On-device/`--snap` visual check on at least one platform to confirm the controls
line up with the bar (the one thing headless can't fully assert).

## Non-goals / out of scope

- A custom bar or custom-drawn controls on **Linux** (keeps native frame).
- Pixel-custom Windows caption-button artwork (uses Rune's, themed).
- The future custom header components themselves — this lands the bar + behaviour
  and the empty, app-owned space for them.

## Risks

- `TITLEBAR_H` vs traffic-light vertical centring and Windows button height — one
  tuning pass on-device.
- `caption_color` correctness in both Day/Night themes and light/dark.
- Not regressing the cross-locale overflow net; the new top strip changes the
  root from a `row` to a `col`, so re-verify `window_w` measurement and mobile.
