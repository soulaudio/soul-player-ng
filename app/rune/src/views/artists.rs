//! Artists grid + an artist's detail page, mirroring Albums (the original's
//! Artists tab). Groups the flat library by artist; each card is a circular
//! avatar with the artist name and a track count, and clicking one opens the
//! artist's detail page (Top Songs + Discography). Reuses albums'
//! `grid::responsive_grid` and `detail_track_row` so the two library pages stay
//! consistent.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use rune::{Cursor, Cx, El, ScrollSpec, col, el, row, text};
use soul_player_host::TrackView;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::{albums_of_artist, detail_track_row};
use crate::views::detail_page::{HeroArt, detail_hero, play_all_pill};
use crate::views::grid;
use crate::views::media_card::{CardSpec, media_card};

/// One grouped artist: display name, a track id to draw a cover from, and every
/// track id (in library order) for Play All / the detail list.
pub(crate) struct Artist {
    name: String,
    cover_id: u32,
    ids: Vec<u32>,
}

/// Group the flat library by artist, preserving first-seen (library) order so
/// the grid follows the baked sort. Tracks with no artist fall under "Unknown".
pub(crate) fn group_artists(library: &[TrackView]) -> Vec<Artist> {
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

/// One artist card: a circular avatar with a hover play-overlay, the name, and a
/// track count. Clicking the body opens the artist's detail page; the overlay
/// plays the artist's tracks. Mirrors the album card (the original's MediaCard).
fn artist_card(s: &State, p: &Palette, slot: usize, a: &Artist, card_w: f32, scale: f32) -> El {
    let name = a.name.clone();
    media_card(
        s,
        p,
        CardSpec {
            key: format!("artist-{slot}"),
            label: a.name.clone(),
            title: a.name.clone(),
            subtitle: Some(t!("artist.trackCount", count => a.ids.len()).to_string()),
            cover_id: a.cover_id,
            circular: true,
            play_ids: a.ids.clone(),
        },
        card_w,
        scale,
        move |st: &mut State| {
            crate::navigate_to(st, crate::Route::Artist, None, Some(name.clone()), None);
            st.artist_top_expanded = false;
        },
    )
}

/// The Artists page: a responsive grid of artist cards, scrolled vertically.
pub fn artists_page(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let artists = group_artists(&s.library);
    // Reuse the albums grid's responsive column rule; read this page's own pane
    // width (first frame has no prior layout → a sensible desktop fallback).
    let pane = cx.layout_size("artists").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;

    let mut grid = row()
        .key("artists-grid")
        .role("grid")
        .flex_wrap()
        .gaps(space::MD, space::GUTTER)
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
        .on_press(crate::go_back)
}

/// An artist's detail page (the original's ArtistPage): a back link, a circular
/// hero ("ARTIST" eyebrow, name, "{n} albums · {n} tracks", Play All), then Top
/// Songs (first five, expandable) and a Discography grid of the artist's albums.
pub fn artist_detail(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
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

    let discography = albums_of_artist(&s.library, &artist.name);
    let meta = t!(
        "detail.albumsAndTracks",
        albums => discography.len(),
        tracks => artist.ids.len()
    )
    .to_string();
    let hero = detail_hero(
        s,
        p,
        scale,
        "artist",
        HeroArt::Cover {
            id: artist.cover_id,
            circular: true,
        },
        t!("artist.eyebrow").to_uppercase(),
        artist.name.clone(),
        None,
        meta,
        play_all_pill("artist-play-all", artist.ids.clone(), p),
    );

    // Top Songs: the first five (or all, expanded), as album-art track rows.
    let shown = if s.artist_top_expanded {
        artist.ids.len()
    } else {
        artist.ids.len().min(5)
    };
    let mut top = col()
        .key("artist-top-songs")
        .gap(space::SM)
        .pb(space::SECTION)
        .child(
            text(t!("artist.topSongs").to_string())
                .font(type_::HEADING)
                .color(p.fg),
        );
    for (i, &id) in artist.ids.iter().take(shown).enumerate() {
        if let Some(t) = s.library.iter().find(|t| t.id == id) {
            top = top.child(detail_track_row(
                s.now_playing_id,
                s.playing,
                p,
                &artist.ids,
                i,
                t,
            ));
        }
    }
    if artist.ids.len() > 5 {
        let label = if s.artist_top_expanded {
            t!("artist.showLess")
        } else {
            t!("artist.showMore")
        };
        top = top.child(
            el().key("artist-top-toggle")
                .role("button")
                .label(label.to_string())
                .focusable(true)
                .px(space::SM)
                .py(space::XS)
                .radius(radius::SM)
                .cursor(Cursor::Pointer)
                .hover(p.fg.alpha(fx::HOVER))
                .press(p.fg.alpha(fx::PRESS))
                .child(text(label.to_string()).font(type_::ITEM).color(p.accent))
                .on_press(|st: &mut State| st.artist_top_expanded = !st.artist_top_expanded),
        );
    }

    // Discography: the artist's albums as a responsive card grid; a card opens the
    // album. Read this section's own committed width for the column count.
    let pane = cx
        .layout_size("artist-discography")
        .map(|[w, _]| w)
        .unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;
    let mut disc = col().key("artist-discography").gap(space::SM).child(
        text(t!("artist.discography").to_string())
            .font(type_::HEADING)
            .color(p.fg),
    );
    if !discography.is_empty() {
        let mut grid_el = row()
            .key("artist-disc-grid")
            .role("grid")
            .flex_wrap()
            .gaps(space::MD, space::GUTTER);
        for (slot, alb) in discography.iter().enumerate() {
            let name = alb.name.clone();
            grid_el = grid_el.child(media_card(
                s,
                p,
                CardSpec {
                    key: format!("artist-disc-{slot}"),
                    label: alb.name.clone(),
                    title: alb.name.clone(),
                    subtitle: None,
                    cover_id: alb.cover_id,
                    circular: false,
                    play_ids: alb.ids.clone(),
                },
                card_w,
                scale,
                move |st: &mut State| {
                    crate::navigate_to(st, crate::Route::Album, Some(name.clone()), None, None);
                },
            ));
        }
        disc = disc.child(grid_el);
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
                .child(hero)
                .child(top)
                .child(disc)
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
            genre: Some("Rock".into()),
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
