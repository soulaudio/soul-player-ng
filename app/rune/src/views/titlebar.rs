//! The app-drawn window title-bar band. Rune's `App::titlebar` (set in `main.rs`)
//! keeps the native window controls — macOS traffic lights (leading edge, over the
//! sidebar) and Windows `_ [] X` caption buttons (trailing edge) — and this draws
//! the band they sit in, over the content. `.window_drag()` moves the window
//! (double-click zooms; the run loop handles it).
//!
//! On Windows we draw our OWN caption glyphs over the native buttons: the native
//! symbols are tinted transparent (`caption_color` alpha 0) so only ours show,
//! while the native buttons underneath still take the clicks (preserving the OS
//! minimize/maximize/close wiring, Aero Snap and the resize edges). Our glyphs
//! fade in and out with the band's hover via `group_opacity` + `hover_motion`,
//! which a per-frame `caption_color` can't do.
#![forbid(unsafe_code)]

use rune::{Cx, El, Icon, Spring, el, icon, row};

use crate::State;
use crate::theme::Palette;
use crate::ui::space;
use crate::views::icons;

/// Title-bar band height, in points.
pub(crate) const HEIGHT: f32 = 32.0;

/// Where the macOS close button's top-left sits (Electron's `trafficLightPosition`):
/// standard left padding, vertically centred for the ~12 pt lights. `main.rs` passes
/// this to `Titlebar::traffic_lights`. macOS only — they sit over the sidebar.
pub(crate) const TRAFFIC_LIGHTS: (f32, f32) = (19.0, (HEIGHT - 12.0) / 2.0);

/// Smooth both-way fade for the hover-revealed Windows caption glyphs.
const GLYPH_FADE: Spring = Spring::new(0.16, 0.0);

/// Caption-glyph icon size, in points. The glyphs' marks fill ~16 of their 24-unit
/// viewbox, so this lands them near the native ~10 pt Windows 11 symbol.
const GLYPH_SIZE: f32 = 15.0;

/// The window title-bar band over the content: `HEIGHT` tall, draggable. On Windows
/// it carries our own `_ [] X` glyphs at the trailing edge, revealed only while the
/// band is hovered; the open area to their left is where custom header components go
/// later.
pub(crate) fn bar(_s: &State, p: &Palette, cx: &mut Cx<State>) -> El {
    let chrome = cx.chrome_insets();
    // Hide the native Windows caption symbols — we draw our own (animatable) glyphs
    // over the buttons, which still take the clicks. alpha 0 keeps macOS unaffected.
    cx.caption_color(p.fg.alpha(0.0));
    cx.titlebar_height(HEIGHT);

    let strip = row()
        .key("titlebar")
        .w_full()
        .h(HEIGHT)
        .fill(p.bg)
        .items_center()
        .pl(space::MD)
        .window_drag()
        // The band is the hover group; its glyphs' `group_opacity` follows this
        // track, and `hover_motion` makes that follow smooth in both directions.
        .hover_group()
        .hover_motion(GLYPH_FADE, GLYPH_FADE)
        .child(
            row()
                .key("titlebar-content")
                .grow(1.0)
                .min_w(0.0)
                .h_full()
                .items_center(),
        );

    // Windows only (`trailing` is 0 where the native frame stays, e.g. macOS/Linux):
    // our three caption glyphs span the trailing gutter, one per native button.
    if chrome.trailing > 0.0 {
        let cell = chrome.trailing / 3.0;
        strip.child(
            row()
                .key("caption-glyphs")
                .h_full()
                .child(caption_glyph("caption-min", &icons::CAPTION_MIN, cell, p))
                .child(caption_glyph("caption-max", &icons::CAPTION_MAX, cell, p))
                .child(caption_glyph(
                    "caption-close",
                    &icons::CAPTION_CLOSE,
                    cell,
                    p,
                )),
        )
    } else {
        strip
    }
}

/// One caption glyph cell: `width` wide, full height, glyph centred, invisible at
/// rest and opaque while the band is hovered (a smooth fade via the band's
/// `hover_motion`). Purely visual — the native button underneath takes the click.
fn caption_glyph(key: &'static str, glyph: &'static Icon, width: f32, p: &Palette) -> El {
    el().key(key)
        .w(width)
        .h_full()
        .center()
        .group_opacity(0.0, 1.0)
        .child(icon(glyph, GLYPH_SIZE).color(p.fg))
}
