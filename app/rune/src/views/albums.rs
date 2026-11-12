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

/// Card width (and cover side — the cover is square and fills the card).
const CARD_W: f32 = 160.0;
/// The hover play-overlay button — a circle, so its radius is half its size.
const PLAY_BTN: f32 = 48.0;

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
fn album_card(s: &State, p: &Palette, slot: usize, a: &Album, scale: f32) -> El {
    let ids = a.ids.clone();
    col()
        .key(format!("album-{slot}"))
        .w(CARD_W)
        .gap(space::SM)
        .padding(space::SNUG)
        .radius(radius::MD)
        .cursor(Cursor::Pointer)
        .hover_group()
        .hover(p.fg.alpha(0.05))
        .press(p.fg.alpha(0.09))
        .child(
            // Cover + a play button that fades in on card hover (old app's
            // `opacity-0 group-hover:opacity-100` MediaCard overlay). The overlay
            // spans the whole cover and carries the play press, so a click
            // anywhere on it plays — not only the 48 pt button.
            el().key("cover-area")
                .size(CARD_W, CARD_W)
                .child(cover(s, Some(a.cover_id), CARD_W, p, scale))
                .child(
                    el().key("play-overlay")
                        .size(CARD_W, CARD_W)
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
                            el().key("play-btn")
                                .size(PLAY_BTN, PLAY_BTN)
                                .radius(PLAY_BTN * 0.5)
                                .fill(Color::BLACK.alpha(0.55))
                                .center()
                                .child(icon(&icons::PLAY, 22.0).color(Color::WHITE)),
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
                        .font(type_::ITEM_STRONG)
                        .color(p.fg)
                        .truncate(),
                )
                .child(
                    text(a.artist.clone())
                        .font(type_::CAPTION)
                        .color(p.text_secondary.alpha(0.7))
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

/// The Albums page: a wrapping grid of album cards, scrolled vertically.
pub fn albums_page(s: &State, p: &Palette, _cx: &Cx<State>, scale: f32) -> El {
    let albums = group_albums(&s.library);
    let mut grid = row()
        .key("albums-grid")
        .flex_wrap()
        .gap(space::GUTTER)
        .padding(space::PANE);
    for (slot, a) in albums.iter().enumerate() {
        grid = grid.child(album_card(s, p, slot, a, scale));
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
