//! Albums grid + an album's detail page, mirroring the old app's
//! AlbumsPage/MediaCard and AlbumPage. Each card is a square cover + title +
//! artist; the hover play button plays/pauses the album, the card body opens its
//! detail page. `TrackView` carries no year, so (unlike the original's "artist •
//! year") the card shows the artist alone.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use rune::{Cursor, Cx, El, ScrollSpec, col, el, row, text};
use soul_player_host::{Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::detail_page::{HeroArt, detail_hero, play_all_pill};
use crate::views::grid;
use crate::views::library::fmt_dur;
use crate::views::media_card::{CardSpec, media_card};
use crate::views::player::remote_badge;

/// One grouped album: display name, its artist, a track id to draw the cover
/// from, and every track id (in library order) to play on click.
pub(crate) struct Album {
    pub(crate) name: String,
    pub(crate) artist: String,
    pub(crate) cover_id: u32,
    pub(crate) ids: Vec<u32>,
}

/// Group the flat library into albums, preserving first-seen (library) order so
/// the grid follows the baked sort. Tracks with no album fall under "Unknown".
pub(crate) fn group_albums(library: &[TrackView]) -> Vec<Album> {
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

/// Sum of track seconds as a coarse human total — "1 h 23 min" or "45 min"
/// (detail headers show totals, not mm:ss). Rounds to whole minutes. Pure.
pub(crate) fn fmt_total_dur(total_secs: f64) -> String {
    let mins = (total_secs.max(0.0) / 60.0).round() as u64;
    let (h, m) = (mins / 60, mins % 60);
    if h > 0 {
        t!("detail.durationHm", hours => h, mins => m).to_string()
    } else {
        t!("detail.durationM", mins => m).to_string()
    }
}

/// The albums of a single artist: filter the library to that artist (matching
/// `group_artists`' empty→"Unknown" bucketing), then fold by album in first-seen
/// order — the artist page's Discography. Pure, reusing the album fold.
pub(crate) fn albums_of_artist(library: &[TrackView], artist: &str) -> Vec<Album> {
    let subset: Vec<TrackView> = library
        .iter()
        .filter(|t| {
            let a = t
                .artist
                .clone()
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| t!("library.unknown").to_string());
            a == artist
        })
        .cloned()
        .collect();
    group_albums(&subset)
}

/// One album card: a square cover with a hover play-overlay, the title, and the
/// artist. Clicking the body opens the album's detail page; the overlay plays.
fn album_card(s: &State, p: &Palette, slot: usize, a: &Album, card_w: f32, scale: f32) -> El {
    let name = a.name.clone();
    media_card(
        s,
        p,
        CardSpec {
            key: format!("album-{slot}"),
            label: a.name.clone(),
            title: a.name.clone(),
            subtitle: Some(a.artist.clone()),
            cover_id: a.cover_id,
            circular: false,
            play_ids: a.ids.clone(),
        },
        card_w,
        scale,
        move |st: &mut State| {
            crate::navigate_to(st, crate::Route::Album, Some(name.clone()), None, None);
        },
    )
}

/// The Albums page: a responsive grid of album cards, scrolled vertically.
pub fn albums_page(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let albums = group_albums(&s.library);
    // Responsive grid (the old app's `grid-cols-2 sm:3 md:4 lg:5 xl:6 2xl:8`): fill
    // the content pane, adding columns as it widens. Read the pane's laid-out width
    // (minus the grid's own padding); the first frame has no prior layout, so fall
    // back to a sensible desktop width.
    let pane = cx.layout_size("albums").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;

    let mut grid = row()
        .key("albums-grid")
        .role("grid")
        .flex_wrap()
        // Rows a touch tighter than columns: each card's caption adds visual
        // whitespace below the tile, so an even gap reads as too much between rows.
        .gaps(space::MD, space::GUTTER)
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

/// A back link — a `←` glyph. Returns to wherever this album was opened from
/// (the back-stack), falling back to the Albums grid if reached without one.
fn back_button(p: &Palette) -> El {
    el().key("album-back")
        .role("button")
        .label(t!("nav.albums").to_string())
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

/// One row on the album detail page: the track number (or a ⏸/▶ state glyph when
/// it's the current track), title and duration. Clicking plays the album from
/// that track, or toggles pause/resume when it is already the one playing.
pub(crate) fn detail_track_row(
    now_playing: Option<u32>,
    is_playing: bool,
    p: &Palette,
    ids: &[u32],
    i: usize,
    t: &TrackView,
) -> El {
    let playing = now_playing == Some(t.id);
    let ids = ids.to_vec();
    let start = i as u32;
    let tid = t.id;
    let mut r = row()
        .key(format!("album-track-{i}"))
        .role("button")
        .label(
            t.title
                .clone()
                .unwrap_or_else(|| t!("library.unknown").to_string()),
        )
        .focusable(true)
        .gap(space::MD)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS));
    if playing {
        // The now-playing row keeps an accent wash (old app's `bg-accent/20`).
        r = r.fill(p.accent.alpha(0.2));
    }
    r = r
        .child(
            text(if playing {
                (if is_playing { "⏸" } else { "▶" }).to_string()
            } else {
                format!("{}", i + 1)
            })
            .font(type_::ITEM)
            .color(if playing { p.accent } else { p.text_secondary })
            .w(28.0),
        )
        .child(
            text(
                t.title
                    .clone()
                    .unwrap_or_else(|| t!("library.unknown").to_string()),
            )
            .font(type_::ITEM)
            .color(if playing { p.accent } else { p.fg })
            .truncate()
            .grow(1.0),
        );
    // A subtle cloud badge for a track streaming from a remote source;
    // nothing renders for a local track, the unremarked default.
    if t.remote {
        r = r.child(remote_badge(t.id, p));
    }
    r.child(
        text(fmt_dur(t.duration_secs))
            .font(type_::ITEM)
            .color(p.text_secondary)
            .w(56.0),
    )
    .on_press(move |st: &mut State| {
        let cmd = if st.now_playing_id == Some(tid) {
            Command::TogglePlay
        } else {
            Command::PlayContext {
                ids: ids.clone(),
                start,
            }
        };
        st.host.send(cmd);
    })
}

/// An album's detail page (the old app's AlbumPage): a back link, then the shared
/// hero — square cover, "ALBUM" eyebrow, title, artist, "{n} tracks · {duration}"
/// and a Play-All pill — over the album's numbered track list. The album is
/// `State::open_album`, re-derived from the current library; if it has vanished we
/// just offer the way back.
pub fn album_detail(s: &State, p: &Palette, scale: f32) -> El {
    let albums = group_albums(&s.library);
    let Some(album) = s
        .open_album
        .as_deref()
        .and_then(|name| albums.into_iter().find(|a| a.name == name))
    else {
        return col()
            .key("album-detail")
            .grow(1.0)
            .min_h(0.0)
            .padding(space::PANE)
            .child(back_button(p));
    };

    let total: f64 = album
        .ids
        .iter()
        .filter_map(|id| s.library.iter().find(|t| t.id == *id))
        .map(|t| t.duration_secs)
        .sum();
    let meta = t!(
        "detail.tracksAndDuration",
        count => album.ids.len(),
        dur => fmt_total_dur(total)
    )
    .to_string();

    let hero = detail_hero(
        s,
        p,
        scale,
        "album",
        HeroArt::Cover {
            id: album.cover_id,
            circular: false,
        },
        t!("album.eyebrow").to_uppercase(),
        album.name.clone(),
        Some(album.artist.clone()),
        meta,
        play_all_pill("album-play-all", album.ids.clone(), p),
    );

    let mut list = col().key("album-tracks").role("list").gap(space::XXS);
    for (i, &id) in album.ids.iter().enumerate() {
        if let Some(t) = s.library.iter().find(|t| t.id == id) {
            list = list.child(detail_track_row(
                s.now_playing_id,
                s.playing,
                p,
                &album.ids,
                i,
                t,
            ));
        }
    }

    col()
        .key("album-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(
            row()
                .key("album-detail-top")
                .px(space::PANE)
                .pt(space::SM)
                .child(back_button(p)),
        )
        .child(
            col()
                .key("album-detail-content")
                .padding(space::PANE)
                .child(hero)
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

    fn atv(id: u32, artist: &str, album: &str) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote: false,
        }
    }

    #[test]
    fn total_duration_rounds_to_whole_minutes_and_formats_hours() {
        rust_i18n::set_locale("en");
        assert_eq!(fmt_total_dur(45.0 * 60.0), "45 min");
        assert_eq!(fmt_total_dur(83.0 * 60.0), "1 h 23 min");
        assert_eq!(fmt_total_dur(0.0), "0 min");
        assert_eq!(fmt_total_dur(29.0), "0 min", "under 30 s rounds down");
        assert_eq!(fmt_total_dur(31.0), "1 min", "over 30 s rounds up");
    }

    #[test]
    fn albums_of_artist_folds_only_that_artists_albums_first_seen() {
        rust_i18n::set_locale("en");
        let lib = vec![
            atv(1, "Bach", "Cello Suites"),
            atv(2, "Satie", "Gymnopedies"),
            atv(3, "Bach", "Cello Suites"),
            atv(4, "Bach", "Goldberg"),
        ];
        let albums = albums_of_artist(&lib, "Bach");
        assert_eq!(albums.len(), 2, "only Bach's two albums");
        assert_eq!(albums[0].name, "Cello Suites");
        assert_eq!(
            albums[0].ids,
            vec![1, 3],
            "both Cello Suites tracks fold in"
        );
        assert_eq!(albums[1].name, "Goldberg");
        assert_eq!(albums[1].ids, vec![4]);
    }
}
