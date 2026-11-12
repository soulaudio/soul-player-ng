//! Albums grid: the library grouped by album into cover cards, mirroring the old
//! app's AlbumsPage/MediaCard. Each card is a square cover + title + artist and
//! plays the whole album on click. `TrackView` carries no year, so (unlike the
//! original's "artist • year") the card shows the artist alone.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use rune::{Color, Cursor, Cx, El, ScrollSpec, col, el, icon, row, text};
use soul_player_host::{Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::views::icons;
use crate::views::player::cover;

/// The hover play-overlay button — the old app's `w-14 h-14 rounded-xl` (56 px).
const PLAY_BTN: f32 = 56.0;
/// Smallest album-card width before the responsive grid drops a column. Tuned so
/// the column count tracks the old app's `grid-cols-2..8` breakpoints and cards
/// land ~144 px at a desktop width (its `lg` tier).
const MIN_CARD: f32 = 130.0;

/// One grouped album: display name, its artist, a track id to draw the cover
/// from, and every track id (in library order) to play on click.
struct Album {
    name: String,
    artist: String,
    cover_id: u32,
    ids: Vec<u32>,
}

/// Group the flat library into albums, preserving first-seen (library) order so
/// the grid follows the baked sort. Tracks with no album fall under "Unknown".
fn group_albums(library: &[TrackView]) -> Vec<Album> {
    let mut albums: Vec<Album> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for t in library {
        let name = t
            .album
            .clone()
            .unwrap_or_else(|| t!("library.unknown").to_string());
        let i = *index.entry(name.clone()).or_insert_with(|| {
            albums.push(Album {
                name,
                artist: t.artist.clone().unwrap_or_default(),
                cover_id: t.id,
                ids: Vec::new(),
            });
            albums.len() - 1
        });
        albums[i].ids.push(t.id);
    }
    albums
}

/// One album card: a square cover, the title, and the artist; clicking plays the
/// album from its first track.
fn album_card(s: &State, p: &Palette, slot: usize, a: &Album, card_w: f32, scale: f32) -> El {
    let ids = a.ids.clone();
    col()
        .key(format!("album-{slot}"))
        .w(card_w)
        // No wrapper background or inset — the old app's MediaCard is `pb-2` with a
        // `mb-2` cover; the cover + the hover play-overlay are the affordance, not a
        // background pill.
        .gap(space::SM)
        .pb(space::SM)
        .cursor(Cursor::Pointer)
        .hover_group()
        .child(
            // Cover + a play button that fades in on card hover (old app's
            // `opacity-0 group-hover:opacity-100` MediaCard overlay). The overlay
            // spans the whole cover and carries the play press, so a click anywhere
            // on it plays — not only the button.
            el().key("cover-area")
                .size(card_w, card_w)
                .child(cover(s, Some(a.cover_id), card_w, radius::MD, p, scale))
                .child(
                    el().key("play-overlay")
                        .size(card_w, card_w)
                        .absolute(0.0, 0.0)
                        .center()
                        .group_opacity(0.0, 1.0)
                        .cursor(Cursor::Pointer)
                        .on_press({
                            let ids = ids.clone();
                            move |st: &mut State| {
                                st.host.send(Command::PlayContext {
                                    ids: ids.clone(),
                                    start: 0,
                                })
                            }
                        })
                        .child(
                            // Old app's `w-14 h-14 rounded-xl bg-black/50` button,
                            // darkening to ~black/70 on direct hover (its
                            // `hover:bg-black/70`).
                            el().key("play-btn")
                                .size(PLAY_BTN, PLAY_BTN)
                                .radius(radius::LG)
                                .fill(Color::BLACK.alpha(0.5))
                                .hover(Color::BLACK.alpha(0.3))
                                .center()
                                .child(icon(&icons::PLAY, 32.0).color(Color::WHITE)),
                        ),
                ),
        )
        .child(
            col()
                .key("album-text")
                .gap(space::XXS)
                .min_w(0.0)
                .child(
                    text(a.name.clone())
                        .font(type_::CARD_TITLE)
                        .color(p.fg)
                        .truncate(),
                )
                .child(
                    text(a.artist.clone())
                        .font(type_::ITEM)
                        .color(p.text_secondary)
                        .truncate(),
                ),
        )
        .on_press(move |st: &mut State| {
            st.host.send(Command::PlayContext {
                ids: ids.clone(),
                start: 0,
            })
        })
}

/// Columns and per-card width for a content pane `pane_w` points wide: fill the
/// pane, dropping a column below [`MIN_CARD`]. Fed the pane width (window minus the
/// sidebar), the column count tracks the old app's `grid-cols-2..8` viewport
/// breakpoints. Pure, so the grid and its test share exactly one rule.
fn grid_layout(pane_w: f32) -> (usize, f32) {
    let avail = (pane_w - 2.0 * space::PANE).max(MIN_CARD);
    let cols = (((avail + space::GUTTER) / (MIN_CARD + space::GUTTER)).floor() as usize).max(1);
    let card_w = ((avail - (cols as f32 - 1.0) * space::GUTTER) / cols as f32).floor();
    (cols, card_w)
}

/// The Albums page: a responsive grid of album cards, scrolled vertically.
pub fn albums_page(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let albums = group_albums(&s.library);
    // Responsive grid (the old app's `grid-cols-2 sm:3 md:4 lg:5 xl:6 2xl:8`): fill
    // the content pane, adding columns as it widens. Read the pane's laid-out width
    // (minus the grid's own padding); the first frame has no prior layout, so fall
    // back to a sensible desktop width.
    let pane = cx.layout_size("albums").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid_layout(pane).1;

    let mut grid = row()
        .key("albums-grid")
        .flex_wrap()
        .gap(space::GUTTER)
        .padding(space::PANE);
    for (slot, a) in albums.iter().enumerate() {
        grid = grid.child(album_card(s, p, slot, a, card_w, scale));
    }
    col().key("albums").grow(1.0).min_h(0.0).child(
        grid.scroll(ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(24.0)
        .grow(1.0)
        .min_h(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_the_pane_with_more_columns_as_it_widens() {
        // Fed the content-pane width (window minus the ~294 pt sidebar), the column
        // count tracks the old app's grid-cols-2..8 viewport breakpoints, and cards
        // land ~144 pt at a desktop width (its `lg` tier).
        assert_eq!(grid_layout(606.0).0, 4, "md-ish pane → 4 columns");
        let (cols, card) = grid_layout(820.0);
        assert_eq!(cols, 5, "lg pane → 5 columns");
        assert!((card - 144.0).abs() <= 2.0, "lg cards ~144 pt, got {card}");
        assert_eq!(grid_layout(1306.0).0, 8, "2xl pane → 8 columns");
        // Never fewer than one column; the card never shrinks below the minimum.
        assert_eq!(grid_layout(200.0).0, 1);
        assert!(grid_layout(820.0).1 >= MIN_CARD);
    }
}
