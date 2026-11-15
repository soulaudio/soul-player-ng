//! The app-drawn window title-bar band. Rune's `App::titlebar` (set in `main.rs`)
//! keeps the native window controls — macOS traffic lights (leading edge, over the
//! sidebar) and Windows `_ [] X` caption buttons (trailing edge) — and this draws
//! the band they sit in, over the content. `.window_drag()` moves the window
//! (double-click zooms; the run loop handles it).
//!
//! On Windows we draw our OWN caption glyphs over the native buttons: the native
//! symbols are tinted transparent (`caption_color` alpha 0) so only ours show,
//! while the native buttons underneath still take the clicks (preserving the OS
//! minimize/maximize/close wiring, Aero Snap and the resize edges). The glyphs
//! are always visible, matching native Windows 11 (which never hides its caption
//! buttons) — see the note on `bar` for why hover-to-reveal can't work here.
#![forbid(unsafe_code)]

use rune::{Cx, El, Icon, el, icon, row, text};
use soul_player_host::{ScanPhase, ScanProgress};

use crate::State;
use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::views::icons;

/// Title-bar band height, in points.
pub(crate) const HEIGHT: f32 = 32.0;

/// Where the macOS close button's top-left sits (Electron's `trafficLightPosition`):
/// standard left padding, vertically centred for the ~12 pt lights. `main.rs` passes
/// this to `Titlebar::traffic_lights`. macOS only — they sit over the sidebar.
pub(crate) const TRAFFIC_LIGHTS: (f32, f32) = (19.0, (HEIGHT - 12.0) / 2.0);

/// Caption-glyph icon size, in points. The glyphs' marks fill ~16 of their 24-unit
/// viewbox, so this lands them near the native ~10 pt Windows 11 symbol.
const GLYPH_SIZE: f32 = 15.0;

/// The window title-bar band over the content: `HEIGHT` tall, draggable. On Windows
/// it carries our own always-visible `_ [] X` glyphs at the trailing edge; the open
/// area to their left is where custom header components go later.
///
/// The glyphs don't hover-reveal: the native caption buttons are a non-client OS
/// region (`WM_NCHITTEST` → `HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE`), so the pointer
/// over them never reaches the app's client-area hover group — a reveal would blank
/// out exactly as you reach for a button. Native Windows keeps them always shown, so
/// we do too.
pub(crate) fn bar(s: &State, p: &Palette, cx: &mut Cx<State>) -> El {
    let chrome = cx.chrome_insets();
    // Hide the native Windows caption symbols — we draw our own (animatable) glyphs
    // over the buttons, which still take the clicks. alpha 0 keeps macOS unaffected.
    cx.caption_color(p.fg.alpha(0.0));
    cx.titlebar_height(HEIGHT);

    // The BAND's own committed width (previous frame) — not the window's. On desktop
    // the band sits inside the shell, right of the resizable sidebar, so its width is
    // `window − sidebar`; measuring it (rather than `app-root`) keeps the widget's
    // responsive variant and its right dock correct at every sidebar/window size.
    let band_w = cx
        .layout_size("titlebar")
        .map(|size| size[0])
        .filter(|w| *w > 0.0)
        .unwrap_or(1100.0);

    // The flexible header slot. While a scan runs it carries the right-aligned
    // activity widget — a leading grow spacer pushes it to the trailing edge, just
    // left of the window controls; otherwise it's the empty draggable gap.
    let mut content = row()
        .key("titlebar-content")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .items_center();
    if let Some(prog) = &s.scan_progress {
        content = content
            .child(el().grow(1.0))
            .child(scan_activity(prog, s.scan_anim, p, band_w))
            .pr(space::SM);
    }

    let strip = row()
        .key("titlebar")
        .w_full()
        .h(HEIGHT)
        .fill(p.bg)
        .items_center()
        .pl(space::MD)
        .window_drag()
        .child(content);

    // Windows only (`trailing` is 0 where the native frame stays, e.g. macOS/Linux):
    // our three always-visible caption glyphs span the trailing gutter, one per
    // native button.
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

/// One caption glyph cell: `width` wide, full height, glyph centred. Always
/// visible. Purely visual — the native button underneath takes the click.
fn caption_glyph(key: &'static str, glyph: &'static Icon, width: f32, p: &Palette) -> El {
    el().key(key)
        .w(width)
        .h_full()
        .center()
        .child(icon(glyph, GLYPH_SIZE).color(p.fg))
}

/// Width-responsive thresholds (band width, pt) for the scan-activity widget: at or
/// above each, one more element joins the icon. Below the narrowest, only the pulsing
/// icon shows (the `smallest width → just the animated icon` case). The band is the
/// strip the widget lives in (`window − sidebar` on desktop), not the whole window.
const SCAN_WIDE: f32 = 700.0; // + phase label
const SCAN_MED: f32 = 520.0; // + progress bar
const SCAN_NARROW: f32 = 380.0; // + done/total count

/// Activity-icon size and progress-bar geometry (pt).
const SCAN_ICON: f32 = 14.0;
const SCAN_BAR_W: f32 = 84.0;
const SCAN_BAR_H: f32 = 4.0;
/// Pulse rate (rad/s) of the activity icon's opacity "breathing".
const SCAN_PULSE_SPEED: f32 = 4.2;

/// The title-bar scan-activity widget: a pulsing activity icon plus — as the band
/// widens — the phase label, a determinate progress bar, and the `done / total`
/// count. Rendered right-aligned in the header slot while a scan runs. `anim` is the
/// free-running pulse phase (seconds) the poll advances; `band_w` (the strip's own
/// width) picks the responsive variant.
fn scan_activity(prog: &ScanProgress, anim: f32, p: &Palette, band_w: f32) -> El {
    // Breathing opacity in [0.5, 1.0] so the icon reads as "working" even between
    // progress ticks (the determinate bar carries the exact amount).
    let icon_op = 0.5 + 0.5 * (0.5 + 0.5 * (anim * SCAN_PULSE_SPEED).sin());

    let mut rowel = row()
        .key("titlebar-scan")
        .h_full()
        .items_center()
        .gap(space::XS)
        .child(
            el().key("titlebar-scan-icon")
                .child(icon(&icons::ACTIVITY, SCAN_ICON).color(p.primary.alpha(icon_op))),
        );

    // Phase label (widest only).
    if band_w >= SCAN_WIDE {
        let label = match prog.phase {
            ScanPhase::Discovering => t!("titlebar.scanning"),
            ScanPhase::Extracting => t!("titlebar.indexing"),
        }
        .to_string();
        rowel = rowel.child(
            text(label)
                .key("titlebar-scan-label")
                .font(type_::CAPTION)
                .color(p.text_secondary),
        );
    }

    // Determinate progress bar (medium and up).
    if band_w >= SCAN_MED {
        let frac = if prog.total > 0 {
            (prog.done as f32 / prog.total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        rowel = rowel.child(
            el().key("titlebar-scan-bar")
                .w(SCAN_BAR_W)
                .h(SCAN_BAR_H)
                .fill(p.muted)
                .radius(radius::FULL)
                .child(
                    el().w(SCAN_BAR_W * frac)
                        .h(SCAN_BAR_H)
                        .fill(p.primary)
                        .radius(radius::FULL),
                ),
        );
    }

    // done / total count (narrow and up).
    if band_w >= SCAN_NARROW {
        let count = format!(
            "{} / {}",
            group_thousands(prog.done),
            group_thousands(prog.total)
        );
        rowel = rowel.child(
            text(count)
                .key("titlebar-scan-count")
                .font(type_::LABEL.mono())
                .color(p.text_secondary),
        );
    }

    rowel
}

/// Group a non-negative integer with ASCII thousands separators ("1,024").
/// Deterministic and locale-independent — the title bar is no place for
/// locale-sensitive number formatting (which would also flake locale-ordered tests).
fn group_thousands(n: usize) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}
