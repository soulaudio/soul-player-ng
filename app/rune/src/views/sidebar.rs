//! Left sidebar: Library/Settings nav, the upcoming queue, and the now-playing
//! panel pinned at the bottom. Resizable (drag the right-edge handle) and
//! collapsible, mirroring the old app's `useResizableSidebar` /
//! `useCollapsibleSidebar` (240–480 px, default 288, snap-collapse under 200).
#![forbid(unsafe_code)]

use rune::tok::type_;
use rune::{Align, Cursor, Cx, Drag, El, Phase, TextStyle, col, el, icon, row, text};

use crate::theme::Palette;
use crate::views::icons;
use crate::views::player::player_panel;
use crate::views::queue::queue_section;
use crate::{Route, State, config};

/// The old app's default sidebar width (`w-72`).
pub const DEFAULT_WIDTH: f32 = 288.0;
/// Smallest drag-resized width (`w-60`), as the old app's `useResizableSidebar`.
pub const MIN_WIDTH: f32 = 240.0;
/// Largest drag-resized width, as the old app's `useResizableSidebar`.
pub const MAX_WIDTH: f32 = 480.0;
/// Dragging the handle in past this snaps the sidebar collapsed (old app's
/// `COLLAPSE_THRESHOLD`); dragging back out past it re-expands.
pub const COLLAPSE_THRESHOLD: f32 = 200.0;
/// Width of the collapsed rail (the old app's `CollapsedSidebarStrip`, `w-3`).
pub const STRIP_WIDTH: f32 = 12.0;
/// Width of the invisible drag strip straddling the sidebar's right edge.
const HANDLE_WIDTH: f32 = 6.0;

/// Nav labels: large + semibold, as the old app's sidebar (`text-xl font-semibold`, 20 px).
const NAV_FONT: TextStyle = TextStyle::new(20.0, 600.0);
/// The app version, shown muted in the footer beside the Settings gear.
const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// A clickable nav entry that switches `State::route`. A large semibold label
/// with a trailing status dot — filled with the primary colour when active, a
/// faint ring otherwise — mirroring the old app's sidebar nav.
fn nav_item(key: &str, label: &str, route: Route, active: bool, p: &Palette) -> El {
    let dot = el().key(format!("{key}-dot")).w(8.0).h(8.0).radius(4.0);
    let dot = if active {
        dot.fill(p.primary)
    } else {
        dot.border_sides([1.0, 1.0, 1.0, 1.0], p.border)
    };
    el().key(key.to_string())
        .w_full()
        .px(12.0)
        .py(4.0)
        .cursor(Cursor::Pointer)
        .child(
            row()
                .w_full()
                .center_y()
                .child(
                    text(label)
                        .font(NAV_FONT)
                        .color(if active { p.primary } else { p.text_secondary })
                        .grow(1.0),
                )
                .child(dot),
        )
        .on_press(move |s: &mut State| s.route = route)
}

pub fn sidebar(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    col()
        .key("sidebar")
        .w(s.sidebar_width)
        .h_full()
        .fill(p.card)
        .border_sides([0.0, 1.0, 0.0, 0.0], p.border)
        // Nav (old app's `p-4 pt-6`): just the page link — Settings lives in the
        // footer gear below, as the original.
        .child(col().key("nav").px(16.0).pt(24.0).pb(16.0).child(nav_item(
            "nav-library",
            "Tracks",
            Route::Library,
            s.route == Route::Library,
            p,
        )))
        .child(queue_section(s, p, scale))
        .child(player_panel(s, p, cx, scale))
        .child(settings_footer(s, p))
}

/// The bottom footer (old app's `SettingsFooter`): a Settings gear on the left,
/// the muted app version on the right, under a top border.
fn settings_footer(s: &State, p: &Palette) -> El {
    let active = s.route == Route::Settings;
    row()
        .key("settings-footer")
        .w_full()
        .border_sides([1.0, 0.0, 0.0, 0.0], p.border)
        .px(12.0)
        .py(8.0)
        .center_y()
        .child({
            let gear = el()
                .key("nav-settings")
                .role("button")
                .label("Settings")
                .focusable(true)
                .px(8.0)
                .py(5.0)
                .radius(6.0)
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut State| s.route = Route::Settings)
                .child(icon(&icons::SETTINGS, 16.0).color(if active {
                    p.bg
                } else {
                    p.text_secondary
                }));
            // On the Settings page the gear gets a filled primary pill so "picked"
            // reads clearly — the same active treatment as the sort/theme buttons;
            // otherwise it just highlights on hover.
            if active {
                gear.fill(p.primary)
            } else {
                gear.hover(p.muted)
            }
        })
        .child(
            text(VERSION)
                .font(type_::MONO_CAPTION.mono())
                .color(p.text_secondary)
                .grow(1.0)
                .align(Align::End),
        )
}

/// The geometry a resize drag resolves to, from the width at press and the
/// pointer travel since. `Some(w)` is the new (clamped) expanded width; `None`
/// means the drag crossed [`COLLAPSE_THRESHOLD`] inward and the sidebar should
/// collapse — leaving the saved width alone so re-opening restores it. Pure, so
/// the drag handler and its tests share exactly one rule.
fn resize_to(width_at_press: f32, dx: f32) -> Option<f32> {
    let target = width_at_press + dx;
    (target >= COLLAPSE_THRESHOLD).then(|| target.clamp(MIN_WIDTH, MAX_WIDTH))
}

/// The invisible drag handle straddling the sidebar's right edge (the old app's
/// `cursor-ew-resize` strip). It is rendered *after* the panel in the row and is
/// always present — collapsed or not — so one continuous gesture can resize,
/// snap closed under [`COLLAPSE_THRESHOLD`], and re-open again. `Drag::data`
/// caches the width at press; `Drag::dx` is pointer travel since (independent of
/// the handle moving with the sidebar), so `data + dx` is the live target width.
pub fn resize_handle() -> El {
    el().key("sidebar-resize")
        .w(HANDLE_WIDTH)
        .h_full()
        .cursor(Cursor::ResizeColumn)
        .on_drag(move |s: &mut State, d: &mut Drag, _tx| match d.phase {
            Phase::Down => {
                d.data = if s.sidebar_collapsed {
                    STRIP_WIDTH
                } else {
                    s.sidebar_width
                };
            }
            Phase::Move => match resize_to(d.data, d.dx) {
                Some(w) => {
                    s.sidebar_width = w;
                    s.sidebar_collapsed = false;
                }
                // A collapse leaves `sidebar_width` untouched so re-opening
                // restores the saved width rather than the rail's 12 px.
                None => s.sidebar_collapsed = true,
            },
            Phase::Up => config::save_sidebar(s.sidebar_width, s.sidebar_collapsed),
        })
}

/// The sidebar collapsed to a thin rail with a centred grabber pill (the old
/// app's `CollapsedSidebarStrip`). Clicking it — or dragging the adjacent
/// [`resize_handle`] back out — re-opens the sidebar to its saved width.
pub fn collapsed_strip(p: &Palette) -> El {
    col()
        .key("sidebar-collapsed")
        .w(STRIP_WIDTH)
        .h_full()
        .fill(p.card)
        .border_sides([0.0, 1.0, 0.0, 0.0], p.border)
        .align(Align::Center)
        .center_y()
        .cursor(Cursor::ResizeColumn)
        .on_press(move |s: &mut State| {
            s.sidebar_collapsed = false;
            config::save_sidebar(s.sidebar_width, false);
        })
        .child(
            el().key("grabber")
                .w(4.0)
                .h(56.0)
                .radius(2.0)
                .fill(p.border),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_widens_and_clamps_to_range() {
        // Dragging out from the default widens by the travel...
        assert_eq!(resize_to(DEFAULT_WIDTH, 100.0), Some(388.0));
        // ...but never past the max or below the min.
        assert_eq!(resize_to(DEFAULT_WIDTH, 300.0), Some(MAX_WIDTH));
        assert_eq!(resize_to(DEFAULT_WIDTH, -60.0), Some(MIN_WIDTH)); // 228 → 240
        assert_eq!(resize_to(DEFAULT_WIDTH, -20.0), Some(268.0)); // still in range
    }

    #[test]
    fn resize_collapses_under_threshold_and_reopens() {
        // Dragging in past the threshold collapses (no width returned, so the
        // saved width survives for re-opening).
        assert_eq!(resize_to(DEFAULT_WIDTH, -100.0), None); // 188 < 200
        // From the collapsed rail, dragging back out past the threshold re-opens.
        assert_eq!(resize_to(STRIP_WIDTH, 50.0), None); // 62 < 200, still shut
        assert_eq!(resize_to(STRIP_WIDTH, 250.0), Some(262.0)); // 262 ≥ 200, opens
    }
}
