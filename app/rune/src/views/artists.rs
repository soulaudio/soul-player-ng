//! Artists grid + an artist's detail page, mirroring Albums (the original's
//! Artists tab). Groups the flat library by artist; each card is a square
//! cover with the artist name and a track count, and clicking one opens the
//! artist's tracks with a Play All. Reuses albums' responsive `grid_layout`
//! and `detail_track_row` so the two library pages stay consistent.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use rune::{Color, Cursor, Cx, El, ScrollSpec, col, el, icon, row, text};
use soul_player_host::{Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::{detail_track_row, grid_layout};
use crate::views::icons;
use crate::views::player::cover;

/// Large cover on the artist detail page (matches the album page's `w-44`).
const DETAIL_COVER: f32 = 176.0;

/// One grouped artist: display name, a track id to draw a cover from, and every
/// track id (in library order) for Play All / the detail list.
struct Artist {
    name: String,
    cover_id: u32,
    ids: Vec<u32>,
}

/// Group the flat library by artist, preserving first-seen (library) order so
/// the grid follows the baked sort. Tracks with no artist fall under "Unknown".
fn group_artists(library: &[TrackView]) -> Vec<Artist> {
    let mut artists: Vec<Artist> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for t in library {
        let name = t
            .artist
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| t!("library.unknown").to_string());
        let i = *index.entry(name.clone()).or_insert_with(|| {
            artists.push(Artist {
                name,
                cover_id: t.id,
                ids: Vec::new(),
            });
            artists.len() - 1
        });
        artists[i].ids.push(t.id);
    }
    artists
}

/// One artist card: a square cover, the name, and a track count. Clicking the
/// card opens the artist's detail page (its track list + Play All).
fn artist_card(s: &State, p: &Palette, slot: usize, a: &Artist, card_w: f32, scale: f32) -> El {
    let name = a.name.clone();
    col()
        .key(format!("artist-{slot}"))
        .w(card_w)
        .gap(space::SM)
        .pb(space::SM)
        .cursor(Cursor::Pointer)
        .hover_group()
        .child(
            el().key("artist-cover-area")
                .size(card_w, card_w)
                .radius(radius::MD)
                .shadow(2.0, 8.0, Color::BLACK.alpha(0.15))
                .child(cover(s, Some(a.cover_id), card_w, radius::MD, p, scale)),
        )
        .child(
            col()
                .key("artist-text")
                .gap(space::XXS)
                .min_w(0.0)
                .child(
                    text(a.name.clone())
                        .font(type_::CARD_TITLE)
                        .color(p.fg)
                        .truncate()
                        .group_opacity(1.0, 0.8),
                )
                .child(
                    text(t!("artist.trackCount", count => a.ids.len()).to_string())
                        .font(type_::ITEM)
                        .color(p.text_secondary)
                        .truncate(),
                ),
        )
        .on_press(move |st: &mut State| {
            st.route = crate::Route::Artist;
            st.open_artist = Some(name.clone());
            st.mobile_show_content = true;
        })
}

/// The Artists page: a responsive grid of artist cards, scrolled vertically.
pub fn artists_page(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let artists = group_artists(&s.library);
    // Reuse the albums grid's responsive column rule; read this page's own pane
    // width (first frame has no prior layout → a sensible desktop fallback).
    let pane = cx.layout_size("artists").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid_layout(pane).1;

    let mut grid = row()
        .key("artists-grid")
        .flex_wrap()
        .gap(space::GUTTER)
        .padding(space::PANE);
    for (slot, a) in artists.iter().enumerate() {
        grid = grid.child(artist_card(s, p, slot, a, card_w, scale));
    }
    col().key("artists").grow(1.0).min_h(0.0).child(
        grid.scroll(ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(24.0)
        .grow(1.0)
        .min_h(0.0),
    )
}

/// A back link to the Artists grid — a `←` glyph.
fn back_button(p: &Palette) -> El {
    el().key("artist-back")
        .role("button")
        .label(t!("nav.artists").to_string())
        .focusable(true)
        .px(space::SM)
        .py(space::XS)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(text("←").font(type_::NAV).color(p.fg))
        .on_press(|st: &mut State| st.route = crate::Route::Artists)
}

/// An artist's detail page: a back link, a header (large cover, "ARTIST"
/// eyebrow, name, track count, Play All), then the artist's track list. The
/// artist is `State::open_artist`, re-derived from the current library; if it
/// has vanished we just offer the way back.
pub fn artist_detail(s: &State, p: &Palette, scale: f32) -> El {
    let artists = group_artists(&s.library);
    let Some(artist) = s
        .open_artist
        .as_deref()
        .and_then(|name| artists.into_iter().find(|a| a.name == name))
    else {
        return col()
            .key("artist-detail")
            .grow(1.0)
            .min_h(0.0)
            .padding(space::PANE)
            .child(back_button(p));
    };

    let play_ids = artist.ids.clone();
    let header = row()
        .key("artist-header")
        .flex_wrap()
        .gap(space::XL)
        .pb(space::SECTION)
        .child(
            el().key("artist-cover")
                .radius(radius::MD)
                .shadow(4.0, 16.0, Color::BLACK.alpha(0.22))
                .child(cover(
                    s,
                    Some(artist.cover_id),
                    DETAIL_COVER,
                    radius::MD,
                    p,
                    scale,
                )),
        )
        .child(
            col()
                .key("artist-meta")
                .gap(space::XS)
                .grow(1.0)
                .min_w(0.0)
                .center_y()
                .child(
                    text(t!("artist.eyebrow").to_uppercase())
                        .font(type_::SECTION)
                        .color(p.text_secondary),
                )
                .child(
                    text(artist.name.clone())
                        .font(type_::DISPLAY)
                        .color(p.fg)
                        .truncate(),
                )
                .child(
                    text(t!("artist.trackCount", count => artist.ids.len()).to_string())
                        .font(type_::CAPTION)
                        .color(p.text_secondary),
                )
                .child(
                    // The same "Play All" pill as the album page — plays the
                    // artist's tracks from the top.
                    el().key("artist-play-all")
                        .px(space::XL)
                        .py(space::MD)
                        .radius(radius::FULL)
                        .fill(p.primary)
                        .cursor(Cursor::Pointer)
                        .hover(p.bg.alpha(fx::HOVER_FILL))
                        .press(p.bg.alpha(fx::PRESS_FILL))
                        .on_press(move |st: &mut State| {
                            st.host.send(Command::PlayContext {
                                ids: play_ids.clone(),
                                start: 0,
                            })
                        })
                        .child(
                            row()
                                .gap(space::SM)
                                .center_y()
                                .child(icon(&icons::PLAY, 20.0).color(p.bg))
                                .child(text(t!("album.playAll").to_string()).color(p.bg)),
                        ),
                ),
        );

    let mut list = col().key("artist-tracks").gap(space::XXS);
    for (i, &id) in artist.ids.iter().enumerate() {
        if let Some(t) = s.library.iter().find(|t| t.id == id) {
            list = list.child(detail_track_row(
                s.now_playing_id,
                s.playing,
                p,
                &artist.ids,
                i,
                t,
            ));
        }
    }

    col()
        .key("artist-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(
            row()
                .key("artist-detail-top")
                .px(space::PANE)
                .pt(space::SM)
                .child(back_button(p)),
        )
        .child(
            col()
                .key("artist-detail-content")
                .padding(space::PANE)
                .child(header)
                .child(list)
                .scroll(ScrollSpec {
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

    fn tv(id: u32, artist: &str) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: (!artist.is_empty()).then(|| artist.to_string()),
            album: Some("Alb".into()),
            duration_secs: 1.0,
        }
    }

    #[test]
    fn group_artists_folds_by_artist_in_first_seen_order() {
        let lib = vec![
            tv(1, "Bach"),
            tv(2, "Satie"),
            tv(3, "Bach"),
            tv(4, ""), // no artist → Unknown
        ];
        let artists = group_artists(&lib);
        // First-seen order: Bach, Satie, Unknown.
        assert_eq!(artists.len(), 3);
        assert_eq!(artists[0].name, "Bach");
        assert_eq!(artists[0].ids, vec![1, 3], "both Bach tracks fold into one");
        assert_eq!(artists[0].cover_id, 1, "the cover is the first-seen track");
        assert_eq!(artists[1].name, "Satie");
        assert_eq!(artists[1].ids, vec![2]);
        // An empty/absent artist falls under the shared Unknown bucket.
        assert_eq!(artists[2].ids, vec![4]);
    }
}
