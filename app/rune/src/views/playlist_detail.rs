//! A playlist's detail page. STUB for Task 7 (routes, state wiring only):
//! Task 9 replaces this body with the real entry list, reorder/remove, cover,
//! and Play All.
#![forbid(unsafe_code)]

use rune::{El, col, text};

use crate::State;
use crate::theme::Palette;
use crate::ui::{space, type_};

/// A playlist's detail page. The open playlist is `State::open_playlist`,
/// resolved against `State::playlists`. STUB, Task 9 fills in the real body.
pub fn page(_s: &State, p: &Palette, _scale: f32) -> El {
    col()
        .key("playlist-detail")
        .grow(1.0)
        .min_h(0.0)
        .padding(space::PANE)
        .child(
            text(t!("nav.playlists").to_string())
                .font(type_::TITLE)
                .color(p.fg),
        )
}
