//! Left sidebar: nav label, upcoming queue, and the now-playing panel
//! pinned at the bottom — the old app's `LeftSidebar` stack
//! (`applications/shared/src/components/LeftSidebar.tsx`: NavBar ->
//! QueueSection -> PlayerPanel). The old app's sidebar is resizable
//! (240-480px, default 288); this milestone fixes it at that default.
#![forbid(unsafe_code)]

use rune::tok::type_;
use rune::{El, col, el, text};

use crate::State;
use crate::theme::Palette;
use crate::views::open_folder::open_folder_button;
use crate::views::player::player_panel;
use crate::views::queue::queue_section;

/// Fixed sidebar width, the old app's default. Threaded into `player_panel`
/// so its seek/volume sliders and transport row size to fit exactly this
/// width instead of a number hand-copied to a second place.
pub const WIDTH: f32 = 288.0;

pub fn sidebar(s: &State, p: &Palette, scale: f32) -> El {
    col()
        .key("sidebar")
        .w(WIDTH)
        .h_full()
        .fill(p.card)
        .border_sides([0.0, 1.0, 0.0, 0.0], p.border)
        .child(
            text("Tracks")
                .font(type_::TITLE)
                .color(p.primary)
                .padding(16.0),
        )
        .child(
            el().px(16.0)
                .py(8.0)
                .child(open_folder_button("Open Folder", p)),
        )
        .child(queue_section(s, p))
        .child(player_panel(s, p, scale, WIDTH))
}
