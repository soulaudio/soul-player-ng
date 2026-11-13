//! Left sidebar: Library/Settings nav, the upcoming queue, and the now-playing
//! panel pinned at the bottom. Resizable (drag the right-edge handle) and
//! collapsible, mirroring the old app's `useResizableSidebar` /
//! `useCollapsibleSidebar` (240–480 px, default 288, snap-collapse under 200).
#![forbid(unsafe_code)]

use rune::{Align, Cursor, Cx, Drag, El, Phase, col, el, icon, row, text};

use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
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
/// Below this logical window width the shell goes single-pane "mobile": the
/// sidebar is the full-width menu and a nav tap flips to full-screen content.
/// Matches the original's `MOBILE_BREAKPOINT` (Tailwind `sm`, 640 px).
pub const MOBILE_BREAKPOINT: f32 = 640.0;
/// Width of the invisible drag strip straddling the sidebar's right edge.
const HANDLE_WIDTH: f32 = 6.0;

/// The app version, shown muted in the footer beside the Settings gear.
const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// A clickable nav entry that switches `State::route`. A large semibold label
/// with a trailing status dot — filled with the primary colour when active, a
/// faint ring otherwise — mirroring the old app's sidebar nav.
fn nav_item(key: &str, label: String, route: Route, active: bool, p: &Palette) -> El {
    let dot = el()
        .key(format!("{key}-dot"))
        .w(8.0)
        .h(8.0)
        .radius(radius::XS);
    let dot = if active {
        dot.fill(p.primary)
    } else {
        dot.border_sides([1.0, 1.0, 1.0, 1.0], p.border)
    };
    el().key(key.to_string())
        .w_full()
        .px(space::MD)
        .py(space::XS)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(
            row()
                .w_full()
                .center_y()
                .child(
                    text(label)
                        .font(type_::NAV)
                        .color(if active { p.primary } else { p.text_secondary })
                        .grow(1.0),
                )
                .child(dot),
        )
        // Switch pages and, on mobile, flip to the content pane (the old app's
        // NavBar: `navigate(path); if (isMobile) setMobileShowContent(true)`).
        // Harmless on desktop, where `mobile_show_content` is ignored.
        .on_press(move |s: &mut State| {
            s.route = route;
            s.mobile_show_content = true;
        })
}

/// The mobile content view's top bar: a back arrow that returns to the sidebar
/// "menu" (the old app's title-bar back → `setMobileShowContent(false)`), next
/// to the current page's title. Desktop never renders this.
pub fn mobile_top_bar(title: String, p: &Palette) -> El {
    row()
        .key("mobile-top-bar")
        .w_full()
        .center_y()
        .gap(space::XS)
        .px(space::SM)
        .py(space::SNUG)
        .border_sides([0.0, 0.0, 1.0, 0.0], p.border)
        .child(
            el().key("mobile-back")
                .role("button")
                .label(t!("common.back").to_string())
                .focusable(true)
                .size(32.0, 32.0)
                .center()
                .radius(radius::MD)
                .cursor(Cursor::Pointer)
                .hover(p.fg.alpha(fx::HOVER))
                .press(p.fg.alpha(fx::PRESS))
                .child(text("←").font(type_::NAV).color(p.fg))
                .on_press(|s: &mut State| s.mobile_show_content = false),
        )
        .child(text(title).font(type_::TITLE).color(p.fg))
}

/// The left sidebar. On desktop it is a fixed, resizable `s.sidebar_width`
/// column with a right border; on mobile (`full_width`) it spans the whole
/// window with no border — the old app's `w-full border-r-0` menu view.
pub fn sidebar(s: &State, p: &Palette, cx: &Cx<State>, scale: f32, full_width: bool) -> El {
    let shell = col().key("sidebar").h_full().fill(p.card);
    let shell = if full_width {
        shell.w_full()
    } else {
        shell
            .w(s.sidebar_width)
            .border_sides([0.0, 1.0, 0.0, 0.0], p.border)
    };
    shell
        // Nav (old app's `p-4 pt-6`): just the page link — Settings lives in the
        // footer gear below, as the original.
        .child(
            col()
                .key("nav")
                .px(space::LG)
                .pt(space::XL)
                .pb(space::LG)
                .child(nav_item(
                    "nav-library",
                    t!("nav.tracks").to_string(),
                    Route::Library,
                    s.route == Route::Library,
                    p,
                ))
                .child(nav_item(
                    "nav-albums",
                    t!("nav.albums").to_string(),
                    Route::Albums,
                    matches!(s.route, Route::Albums | Route::Album),
                    p,
                ))
                .child(nav_item(
                    "nav-artists",
                    t!("nav.artists").to_string(),
                    Route::Artists,
                    matches!(s.route, Route::Artists | Route::Artist),
                    p,
                )),
        )
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
        .px(space::MD)
        .py(space::SM)
        .center_y()
        .child({
            let gear = el()
                .key("nav-settings")
                .role("button")
                .label(t!("settings.title").to_string())
                .focusable(true)
                .padding(space::SNUG)
                .radius(radius::MD)
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut State| {
                    s.route = Route::Settings;
                    s.mobile_show_content = true;
                })
                .child(icon(&icons::SETTINGS, 16.0).color(if active {
                    p.primary
                } else {
                    p.text_secondary
                }));
            // 1:1 with the old app's SettingsFooter button: on /settings a primary
            // icon on a subtle accent-20% pill (`text-primary bg-accent/20`),
            // otherwise a faint foreground-10% hover (`hover:bg-foreground/10`).
            if active {
                gear.fill(p.accent.alpha(0.2))
            } else {
                gear.hover(p.fg.alpha(fx::HOVER))
                    .press(p.fg.alpha(fx::PRESS))
            }
        })
        .child(
            text(VERSION)
                .font(type_::LABEL.mono())
                .color(p.text_secondary.alpha(0.6))
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

/// The sidebar's resize handle, keyed `"sidebar-resize"` in BOTH states so one
/// continuous drag survives a collapse↔expand mid-gesture (the element keeps its
/// identity, so the drag capture doesn't drop). `Drag::data` caches the width at
/// press; `Drag::dx` is pointer travel since, so `data + dx` is the live target.
///
/// - **Expanded:** a thin invisible strip on the right edge, primary-tinted under
///   the pointer (the old app's `group-hover:bg-primary/30` handle).
/// - **Collapsed:** a `STRIP_WIDTH`-wide *invisible* drag column (full-height so
///   it stays easy to grab) carrying a short **floating grabber pill** centred
///   vertically — not a full-height card rail. Dragging it **live-resizes** (the
///   sidebar follows the cursor, as the old app's "Drag right: live resize") and
///   a click re-opens to the saved width. Because this element is the drag
///   target, re-opening follows the pointer instead of waiting for release.
pub fn resize_handle(collapsed: bool, p: &Palette) -> El {
    let base = el()
        .key("sidebar-resize")
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
                // Dragged in past the threshold: collapse, leaving `sidebar_width`
                // untouched so re-opening restores the saved width, not the rail's.
                None => s.sidebar_collapsed = true,
            },
            Phase::Up => {
                // A click (or sub-threshold drag) that STARTED collapsed re-opens
                // to the saved width — the old app's click-to-expand. A real
                // drag-out already expanded live above.
                if s.sidebar_collapsed && d.data <= STRIP_WIDTH {
                    s.sidebar_collapsed = false;
                }
                config::save_sidebar(s.sidebar_width, s.sidebar_collapsed);
            }
        });
    if collapsed {
        // A transparent full-height drag column with only a short pill showing —
        // the collapsed sidebar reads as a floating handle, not a full-height bar.
        base.w(STRIP_WIDTH)
            // `center()` centres the pill on BOTH axes. The pill brightens on hover
            // (old app's `group-hover:bg-foreground/35`).
            .center()
            .hover_group()
            .child(
                // The grabber pill: brighter at rest than the old full-card rail's
                // (there's no card behind it now), lifting to full strength on
                // hover via `group_opacity`.
                el().key("grabber")
                    .w(4.0)
                    .h(56.0)
                    .radius(radius::FULL)
                    .fill(p.fg.alpha(0.5))
                    .group_opacity(0.4, 1.0),
            )
    } else {
        base.w(HANDLE_WIDTH).hover(p.primary.alpha(0.3))
    }
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
