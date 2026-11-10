//! Left sidebar: Library/Settings nav, the upcoming queue, and the now-playing
//! panel pinned at the bottom.
#![forbid(unsafe_code)]

use rune::{Cx, El, col, el, text};

use crate::theme::Palette;
use crate::views::player::player_panel;
use crate::views::queue::queue_section;
use crate::{Route, State};

/// Fixed sidebar width, the old app's default.
pub const WIDTH: f32 = 288.0;

/// A clickable nav entry that switches `State::route`; the active route is shown
/// in the primary color.
fn nav_item(key: &str, label: &str, route: Route, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .px(16.0)
        .py(10.0)
        .child(text(label).color(if active { p.primary } else { p.text_secondary }))
        .on_press(move |s: &mut State| s.route = route)
}

pub fn sidebar(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    col()
        .key("sidebar")
        .w(WIDTH)
        .h_full()
        .fill(p.card)
        .border_sides([0.0, 1.0, 0.0, 0.0], p.border)
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
