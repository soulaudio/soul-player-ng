//! Left sidebar: Library/Settings nav, the upcoming queue, and the now-playing
//! panel pinned at the bottom.
#![forbid(unsafe_code)]

use rune::{Cx, El, TextStyle, col, el, row, text};

use crate::theme::Palette;
use crate::views::player::player_panel;
use crate::views::queue::queue_section;
use crate::{Route, State};

/// Fixed sidebar width, the old app's default.
pub const WIDTH: f32 = 288.0;

/// Nav labels: large + semibold, as the old app's sidebar (`text-xl font-semibold`).
const NAV_FONT: TextStyle = TextStyle::new(18.0, 600.0);

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
        .px(16.0)
        .py(6.0)
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
        .w(WIDTH)
        .h_full()
        .fill(p.card)
        .border_sides([0.0, 1.0, 0.0, 0.0], p.border)
        .pt(12.0)
        .child(nav_item(
            "nav-library",
            "Tracks",
            Route::Library,
            s.route == Route::Library,
            p,
        ))
        .child(nav_item(
            "nav-settings",
            "Settings",
            Route::Settings,
            s.route == Route::Settings,
            p,
        ))
        .child(queue_section(s, p))
        .child(player_panel(s, p, cx, scale))
}
