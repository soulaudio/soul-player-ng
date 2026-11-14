//! Genres browse: a list of genre buckets (name + track count) that drills into
//! a genre's tracks. Genres carry no cover art, so this mirrors the **Artists**
//! interaction (a list of names that opens a detail page with Play All) rather
//! than the album-art grid. Groups the flat library by genre; the backend's
//! `BrowseAxis::Genre` order already clusters a genre's tracks together, and the
//! importer's "Unknown Genre" is a real, keep-able bucket. Reuses albums'
//! `detail_track_row` so the detail list stays consistent with the other pages.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use rune::{Cursor, El, ScrollSpec, col, el, icon, row, text};
use soul_player_host::{Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::detail_track_row;
use crate::views::icons;

/// One grouped genre: its display name and every track id (in library order) for
/// Play All / the detail list.
pub(crate) struct Genre {
    name: String,
    ids: Vec<u32>,
}

/// Group the flat library by genre, preserving first-seen (library) order so the
/// list follows the baked `Genre` axis clustering. A track with no genre value
/// falls under the shared "Unknown" bucket (the backend normally supplies the
/// real "Unknown Genre" bucket, so this only guards a resolve miss).
pub(crate) fn group_genres(library: &[TrackView]) -> Vec<Genre> {
    let mut genres: Vec<Genre> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for t in library {
        let name = t
            .genre
            .clone()
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| t!("library.unknown").to_string());
        let i = *index.entry(name.clone()).or_insert_with(|| {
            genres.push(Genre {
                name,
                ids: Vec::new(),
            });
            genres.len() - 1
        });
        genres[i].ids.push(t.id);
    }
    genres
}

/// One genre row: the genre name and its track count. Clicking the row opens the
/// genre's detail page (its track list + Play All) — no cover, so it reads as a
/// list entry rather than a card.
fn genre_row(p: &Palette, slot: usize, g: &Genre) -> El {
    let name = g.name.clone();
    row()
        .key(format!("genre-{slot}"))
        .role("button")
        .label(g.name.clone())
        .focusable(true)
        .w_full()
        .gap(space::MD)
        .center_y()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(
            text(g.name.clone())
                .font(type_::ITEM)
                .color(p.fg)
                .truncate()
                .grow(1.0),
        )
        .child(
            text(t!("genre.trackCount", count => g.ids.len()).to_string())
                .font(type_::ITEM)
                .color(p.text_secondary),
        )
        .on_press(move |st: &mut State| {
            st.route = crate::Route::Genre;
            st.open_genre = Some(name.clone());
            st.mobile_show_content = true;
        })
}

/// The Genres page: a vertical list of genre rows (name + count), scrolled.
pub fn genres_page(s: &State, p: &Palette) -> El {
    let genres = group_genres(&s.library);
    let mut list = col()
        .key("genres-list")
        .role("list")
        .gap(space::XXS)
        .padding(space::PANE);
    for (slot, g) in genres.iter().enumerate() {
        list = list.child(genre_row(p, slot, g));
    }
    col().key("genres").grow(1.0).min_h(0.0).child(
        list.scroll(ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(24.0)
        .grow(1.0)
        .min_h(0.0),
    )
}

/// A back link to the Genres list — a `←` glyph.
fn back_button(p: &Palette) -> El {
    el().key("genre-back")
        .role("button")
        .label(t!("nav.genres").to_string())
        .focusable(true)
        .px(space::SM)
        .py(space::XS)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(text("←").font(type_::NAV).color(p.fg))
        .on_press(|st: &mut State| st.route = crate::Route::Genres)
}

/// A genre's detail page: a back link, a header (a "GENRE" eyebrow, the name, a
/// track count and a Play All — no cover, since genres carry none), then the
/// genre's track list. The genre is `State::open_genre`, re-derived from the
/// current library; if it has vanished we just offer the way back.
pub fn genre_detail(s: &State, p: &Palette) -> El {
    let genres = group_genres(&s.library);
    let Some(genre) = s
        .open_genre
        .as_deref()
        .and_then(|name| genres.into_iter().find(|g| g.name == name))
    else {
        return col()
            .key("genre-detail")
            .grow(1.0)
            .min_h(0.0)
            .padding(space::PANE)
            .child(back_button(p));
    };

    let play_ids = genre.ids.clone();
    let header = col()
        .key("genre-header")
        .gap(space::XS)
        .pb(space::SECTION)
        .child(
            text(t!("genre.eyebrow").to_uppercase())
                .font(type_::SECTION)
                .color(p.text_secondary),
        )
        .child(
            text(genre.name.clone())
                .font(type_::DISPLAY)
                .color(p.fg)
                .truncate(),
        )
        .child(
            text(t!("genre.trackCount", count => genre.ids.len()).to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary),
        )
        .child(
            // The same "Play All" pill as the artist/album pages — plays the
            // genre's tracks from the top.
            el().key("genre-play-all")
                .role("button")
                .label(t!("album.playAll").to_string())
                .focusable(true)
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
        );

    let mut list = col().key("genre-tracks").role("list").gap(space::XXS);
    for (i, &id) in genre.ids.iter().enumerate() {
        if let Some(t) = s.library.iter().find(|t| t.id == id) {
            list = list.child(detail_track_row(
                s.now_playing_id,
                s.playing,
                p,
                &genre.ids,
                i,
                t,
            ));
        }
    }

    col()
        .key("genre-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(
            row()
                .key("genre-detail-top")
                .px(space::PANE)
                .pt(space::SM)
                .child(back_button(p)),
        )
        .child(
            col()
                .key("genre-detail-content")
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

    fn tv(id: u32, genre: Option<&str>) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some("A".into()),
            album: Some("Alb".into()),
            genre: genre.map(|g| g.to_string()),
            duration_secs: 1.0,
        }
    }

    #[test]
    fn group_genres_folds_by_genre_in_first_seen_order() {
        let lib = vec![
            tv(1, Some("Rock")),
            tv(2, Some("Jazz")),
            tv(3, Some("Rock")),
            tv(4, None), // no genre → Unknown
        ];
        let genres = group_genres(&lib);
        // First-seen order: Rock, Jazz, Unknown.
        assert_eq!(genres.len(), 3);
        assert_eq!(genres[0].name, "Rock");
        assert_eq!(genres[0].ids, vec![1, 3], "both Rock tracks fold into one");
        assert_eq!(genres[1].name, "Jazz");
        assert_eq!(genres[1].ids, vec![2]);
        // A None/blank genre falls under the shared Unknown bucket.
        assert_eq!(genres[2].ids, vec![4]);
    }
}
