//! Main-area track list: one row per library track, click to play.
#![forbid(unsafe_code)]

use rune::tok::type_;
use rune::{El, ScrollSpec, col, row, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::views::open_folder::open_folder_button;

fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn track_row(s: &State, p: &Palette, index: usize) -> El {
    let t = &s.library[index];
    let playing = s.now_playing_id == Some(t.id);
    let ids: Vec<u32> = s.library.iter().map(|x| x.id).collect();
    let start = index as u32;
    row()
        .key(format!("track-{}", t.id))
        .gap(12.0)
        .padding(8.0)
        .child(
            if playing {
                // ▶/⏸ are already proven-good glyphs in this font (the
                // transport's play/pause button uses the same pair), so the
                // now-playing row swaps its index for one of them instead of
                // introducing an unverified glyph.
                text(if s.playing { "▶" } else { "⏸" }).color(p.accent)
            } else {
                text(format!("{}", index + 1)).color(p.text_secondary)
            }
            .w(28.0),
        )
        .child(
            text(t.title.clone().unwrap_or_else(|| "Unknown".into()))
                .color(if playing { p.accent } else { p.fg })
                .grow(1.0),
        )
        .child(
            text(t.artist.clone().unwrap_or_default())
                .color(p.text_secondary)
                .grow(1.0),
        )
        .child(
            text(t.album.clone().unwrap_or_default())
                .color(p.text_secondary)
                .grow(1.0),
        )
        .child(
            text(fmt_dur(t.duration_secs))
                .color(p.text_secondary)
                .w(56.0),
        )
        .on_press(move |st: &mut State| {
            st.host.send(Command::PlayContext {
                ids: ids.clone(),
                start,
            })
        })
}

/// The library's ~38-track testdata set fits comfortably in a plain scrolled
/// column (`.scroll`); `El::virtualize`/`virtual_rows` (see
/// `rune::el::virtual_rows`) is the path to reach for once a real-world
/// library runs into the hundreds or thousands of tracks, since it only lays
/// out/shapes text for the rows in (and just around) the viewport.
///
/// `.grow(1.0).min_h(0.0)` make the viewport take its bounded parent's full
/// remaining height (rather than its content's natural height) so it scrolls
/// instead of overflowing — see `El::scroll`'s doc comment.
pub fn track_list(s: &State, p: &Palette) -> El {
    if s.library.is_empty() {
        // Cold launch with no SOUL_PLAYER_FOLDER: a usable entry point rather
        // than a blank list.
        return col()
            .key("empty")
            .w_full()
            .h_full()
            .center()
            .gap(12.0)
            .child(text("No music loaded").font(type_::TITLE).color(p.fg))
            .child(text("Open a folder to start playing").color(p.text_secondary))
            .child(open_folder_button("Open Folder", p));
    }
    let mut list = col().key("tracks").gap(2.0);
    for i in 0..s.library.len() {
        list = list.child(track_row(s, p, i));
    }
    list.scroll(ScrollSpec {
        stick_bottom: false,
    })
    .grow(1.0)
    .min_h(0.0)
}
