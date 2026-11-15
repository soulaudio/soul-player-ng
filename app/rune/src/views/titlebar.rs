//! The app-drawn window title bar. Rune's `App::titlebar` (set in `main.rs`) keeps
//! the native window controls — macOS traffic lights at the leading edge, Windows
//! `_ [] X` caption buttons at the trailing edge, Linux's native frame — and this
//! draws the bar they sit in. `cx.chrome_insets()` says how much space those
//! controls take at each end; the content keeps clear of both. `.window_drag()`
//! makes the strip move the window (double-click zooms); the run loop handles it.
#![forbid(unsafe_code)]

use rune::{Cx, El, row, text};

use crate::State;
use crate::theme::Palette;
use crate::ui::{space, type_};

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
