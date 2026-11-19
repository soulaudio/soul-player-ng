//! Playlists browse: every user-created playlist. STUB for Task 7 (routes,
//! state wiring only): Task 8 replaces this body with the real cover-mosaic
//! list and create/rename/delete actions.
#![forbid(unsafe_code)]

use rune::{El, col, text};

use crate::State;
use crate::theme::Palette;
use crate::ui::{space, type_};

/// The Playlists page. STUB, Task 8 fills in the real list.
pub fn page(_s: &State, p: &Palette) -> El {
    col()
        .key("playlists")
        .grow(1.0)
        .min_h(0.0)
        .padding(space::PANE)
        .child(
            text(t!("nav.playlists").to_string())
                .font(type_::TITLE)
                .color(p.fg),
        )
}
