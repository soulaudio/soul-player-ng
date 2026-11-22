//! Artists grid + an artist's detail page, mirroring Albums (the original's
//! Artists tab). Groups the flat library by artist; each card is a circular
//! avatar with the artist name and a track count, and clicking one opens the
//! artist's detail page (Top Songs + Discography). Reuses albums'
//! `grid::responsive_grid` and `detail_track_row` so the two library pages stay
//! consistent.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use rune::{Color, Cursor, Cx, El, ScrollSpec, col, el, icon, row, text};
use soul_player_host::TrackView;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::{Album, albums_of_artist, detail_track_row};
use crate::views::detail_page::{ArtworkTarget, HeroArt, detail_actions_with_artwork, detail_hero};
use crate::views::grid;
use crate::views::icons;
use crate::views::media_card::{CardSpec, media_card};
use crate::views::player::{cover, cover_from_path, group_remote_source};

/// One grouped artist: display name, a track id to draw a cover from, every
/// track id (in library order) for Play All / the detail list, and whether
/// every one of its tracks streams from the same remote host (`None` for a
/// local or mixed-source artist — see [`group_remote_source`]).
pub(crate) struct Artist {
    name: String,
    cover_id: u32,
    ids: Vec<u32>,
    remote_source: Option<String>,
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
                remote_source: None, // resolved below, once every member track is known
            });
            artists.len() - 1
        });
        artists[i].ids.push(t.id);
    }
    // See `group_albums`' matching second pass: a track id -> remote_source map
    // avoids an O(n²) `library.iter().find` per id.
    let remote_by_id: HashMap<u32, &Option<String>> =
        library.iter().map(|t| (t.id, &t.remote_source)).collect();
    for artist in &mut artists {
        artist.remote_source = group_remote_source(
            artist
                .ids
                .iter()
                .filter_map(|id| remote_by_id.get(id).copied()),
        );
    }
    artists
}

/// Narrow `artists` to those whose name contains `query`, a case-insensitive
/// substring match; `query` must already be lowercased (the caller lowercases
/// the search box's text once rather than per-artist). An empty query keeps
/// every artist, same convention as the genre-detail search.
pub(crate) fn filter_artists(artists: Vec<Artist>, query: &str) -> Vec<Artist> {
    if query.is_empty() {
        return artists;
    }
    artists
        .into_iter()
        .filter(|a| a.name.to_lowercase().contains(query))
        .collect()
}

/// One artist card: a circular avatar with a hover play-overlay, the name, and
/// an "{n} albums · {n} tracks" subtitle (the original's MediaCard). Clicking
/// the body opens the artist's detail page; the overlay plays the artist's
/// tracks.
fn artist_card(s: &State, p: &Palette, slot: usize, a: &Artist, card_w: f32, scale: f32) -> El {
    let name = a.name.clone();
    let album_count = albums_of_artist(&s.library, &a.name, s.album_year).len();
    media_card(
        s,
        p,
        CardSpec {
            key: format!("artist-{slot}"),
            label: a.name.clone(),
            title: a.name.clone(),
            subtitle: Some(format!(
                "{} · {}",
                crate::views::library::albums_label(album_count),
                crate::views::library::tracks_label(a.ids.len())
            )),
            cover_id: a.cover_id,
            cover_override: s.covers.artist_cover_path(&a.name),
            circular: true,
            play_ids: a.ids.clone(),
            remote_source: a.remote_source.clone(),
            dr: None, // artists span albums; DR is an album-level indicator
        },
        card_w,
        scale,
        move |st: &mut State| {
            crate::navigate_to(
                st,
                crate::Route::Artist,
                None,
                Some(name.clone()),
                None,
                None,
            );
            st.artist_top_expanded = false;
        },
    )
}

/// The Artists page: an always-visible search box above a responsive grid of
/// artist cards, scrolled vertically. Mirrors the original soul-player's
/// per-page search (and the genre-detail page's own search box, which this
/// copies the styling of).
pub fn artists_page(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let q = s.artist_search.text.to_lowercase();
    let artists = filter_artists(group_artists(&s.library), &q);
    // Reuse the albums grid's responsive column rule; read this page's own pane
    // width (first frame has no prior layout → a sensible desktop fallback).
    let pane = cx.layout_size("artists").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;

    let search = crate::views::library::search_box(
        &s.artist_search,
        "artists-search",
        t!("artists.searchPlaceholder").to_string(),
        p,
        |st: &mut State| &mut st.artist_search,
    )
    .w_full();
    let search_bar = row()
        .key("artists-search-bar")
        .px(space::PANE)
        .pt(space::SM)
        .child(search);

    let mut grid = row()
        .key("artists-grid")
        .role("grid")
        .flex_wrap()
        .gaps(space::MD, space::GUTTER)
        .padding(space::PANE);
    for (slot, a) in artists.iter().enumerate() {
        grid = grid.child(artist_card(s, p, slot, a, card_w, scale));
    }
    col()
        .key("artists")
        .grow(1.0)
        .min_h(0.0)
        .child(search_bar)
        .child(
            // Scroll on the growing wrapper, not the grid, see `albums_page`: a flex-wrap
            // grid that is itself the growing scroll element stretches its wrapped rows
            // down the viewport when they don't fill it. Natural-height grid, growing
            // scroll viewport around it.
            col()
                .key("artists-scroll")
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .scroll_edge_fade(24.0)
                .grow(1.0)
                .min_h(0.0)
                .child(grid),
        )
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
            .padding(space::PANE);
    };

    let discography = albums_of_artist(&s.library, &artist.name, s.album_year);
    let meta = format!(
        "{} · {}",
        crate::views::library::albums_label(discography.len()),
        crate::views::library::tracks_label(artist.ids.len())
    );
    // A user-uploaded custom avatar (the old app's "Edit Artwork") wins over the
    // cover-track art. Note: `HeroArt::Image` renders a square `radius::MD`
    // tile, so an overridden artist hero is square (not the circular avatar the
    // generated/embedded art uses); the circular grid card still applies the
    // override. An acceptable minor deviation (the custom image is shown as-is).
    let art = match s.covers.artist_cover_path(&artist.name) {
        Some(path) => HeroArt::Image(path),
        None => HeroArt::Cover {
            id: artist.cover_id,
            circular: true,
        },
    };
    let hero = detail_hero(
        s,
        p,
        scale,
        "artist",
        art,
        t!("artist.eyebrow").to_uppercase(),
        artist.name.clone(),
        artist.remote_source.clone(),
        None,
        meta,
        None, // no clock: artist meta is "albums, tracks", not a duration
        detail_actions_with_artwork(
            s,
            p,
            "artist-play-all",
            artist.ids.clone(),
            ArtworkTarget::Artist(artist.name.clone()),
        ),
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
                false, // one artist: artist column is constant
                true,  // but the top songs span albums
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

    // Discography: a grid/list toggle in the header, then either the responsive
    // card grid (default) or a compact row list.
    let pane = cx
        .layout_size("artist-discography")
        .map(|[w, _]| w)
        .unwrap_or(820.0);
    let card_w = grid::columns(pane, grid::MIN_CARD, space::GUTTER).1;
    let toggle = |key: &str, glyph: &'static rune::Icon, label: String, active: bool| {
        let want_list = key == "disc-toggle-list";
        el().key(key.to_string())
            .role("button")
            .label(label)
            .focusable(true)
            .px(space::SNUG)
            .py(space::XS)
            .radius(radius::SM)
            .cursor(Cursor::Pointer)
            .fill(if active {
                p.fg.alpha(fx::SELECTED)
            } else {
                Color::TRANSPARENT
            })
            .hover(p.fg.alpha(fx::HOVER))
            .child(icon(glyph, 18.0).color(if active { p.fg } else { p.text_secondary }))
            .on_press(move |st: &mut State| st.discography_list = want_list)
    };
    let header = row()
        .key("artist-disc-header")
        .center_y()
        .child(
            text(t!("artist.discography").to_string())
                .font(type_::HEADING)
                .color(p.fg)
                .grow(1.0),
        )
        .child(toggle(
            "disc-toggle-grid",
            &icons::GRID,
            t!("artist.viewGrid").to_string(),
            !s.discography_list,
        ))
        .child(toggle(
            "disc-toggle-list",
            &icons::LIST,
            t!("artist.viewList").to_string(),
            s.discography_list,
        ));
    let mut disc = col().key("artist-discography").gap(space::SM).child(header);
    if !discography.is_empty() {
        if s.discography_list {
            let mut list = col().key("artist-disc-list").role("list").gap(space::XXS);
            for (i, alb) in discography.iter().enumerate() {
                list = list.child(album_row(s, p, i, alb, scale));
            }
            disc = disc.child(list);
        } else {
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
                        cover_override: s.covers.album_cover_path(&alb.name),
                        circular: false,
                        play_ids: alb.ids.clone(),
                        remote_source: alb.remote_source.clone(),
                        dr: alb.dr,
                    },
                    card_w,
                    scale,
                    move |st: &mut State| {
                        crate::navigate_to(
                            st,
                            crate::Route::Album,
                            Some(name.clone()),
                            None,
                            None,
                            None,
                        );
                    },
                ));
            }
            disc = disc.child(grid_el);
        }
    }

    col().key("artist-detail").grow(1.0).min_h(0.0).child(
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

/// One Discography row (list mode): a small square cover, the album name, and
/// "{n} tracks · {duration}". Opens the album via the back-stack.
fn album_row(s: &State, p: &Palette, i: usize, alb: &Album, scale: f32) -> El {
    let total: f64 = alb
        .ids
        .iter()
        .filter_map(|id| s.library.iter().find(|t| t.id == *id))
        .map(|t| t.duration_secs)
        .sum();
    let meta = format!(
        "{} · {}",
        crate::views::library::tracks_label(alb.ids.len()),
        crate::views::albums::fmt_total_dur(total)
    );
    let name = alb.name.clone();
    row()
        .key(format!("artist-disc-row-{i}"))
        .role("button")
        .label(alb.name.clone())
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
        .child(match s.covers.album_cover_path(&alb.name) {
            // A custom cover wins here too, so the discography list thumbnail
            // matches the album's grid card and detail hero.
            Some(path) => cover_from_path(s, &path, 48.0, radius::SM, p, scale),
            None => cover(s, Some(alb.cover_id), 48.0, radius::SM, p, scale),
        })
        .child(
            col()
                .gap(space::XXS)
                .min_w(0.0)
                .grow(1.0)
                .child(
                    text(alb.name.clone())
                        .font(type_::ITEM)
                        .color(p.fg)
                        .truncate(),
                )
                .child(
                    text(meta)
                        .font(type_::CAPTION)
                        .color(p.text_secondary)
                        .truncate(),
                ),
        )
        .on_press(move |st: &mut State| {
            crate::navigate_to(
                st,
                crate::Route::Album,
                Some(name.clone()),
                None,
                None,
                None,
            );
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tx;

    fn tv(id: u32, artist: &str) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: (!artist.is_empty()).then(|| artist.to_string()),
            album: Some("Alb".into()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
        }
    }

    /// Like `tv`, but on a named album, so an artist can be spread across
    /// several albums for the "{n} albums · {n} tracks" subtitle test.
    fn tv_album(id: u32, artist: &str, album: &str) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some(artist.to_string()),
            album: Some(album.to_string()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
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

    /// An artist's `remote_source` is the unanimous host across every one of
    /// its tracks; a mix of remote and local (or two different hosts) has no
    /// consensus and must show no badge.
    #[test]
    fn group_artists_resolves_remote_source_consensus() {
        let mut remote1 = tv(1, "Bach");
        remote1.remote_source = Some("h".to_string());
        let mut remote2 = tv(2, "Bach");
        remote2.remote_source = Some("h".to_string());
        let all_remote = group_artists(&[remote1, remote2]);
        assert_eq!(
            all_remote[0].remote_source,
            Some("h".to_string()),
            "every track shares the same host"
        );

        let mut remote = tv(3, "Satie");
        remote.remote_source = Some("h".to_string());
        let local = tv(4, "Satie");
        let mixed = group_artists(&[remote, local]);
        assert_eq!(
            mixed[0].remote_source, None,
            "a local track in the mix breaks consensus"
        );

        let all_local = group_artists(&[tv(5, "Satie"), tv(6, "Satie")]);
        assert_eq!(all_local[0].remote_source, None, "all local -> no badge");
    }

    /// `filter_artists` keeps an artist when the query substring-matches its
    /// name (case-insensitively), and keeps everyone for an empty query.
    #[test]
    fn filter_artists_matches_name_case_insensitively() {
        let lib = [tv(1, "Bach"), tv(2, "Satie"), tv(3, "Beck")];
        let matched = filter_artists(group_artists(&lib), "ba");
        assert_eq!(
            matched.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["Bach"],
            "a lowercased substring query keeps only the matching artist"
        );
        let empty_query = filter_artists(group_artists(&lib), "");
        assert_eq!(empty_query.len(), 3, "an empty query keeps every artist");
        let no_match = filter_artists(group_artists(&lib), "zzz");
        assert!(no_match.is_empty(), "no match keeps nobody");
    }

    /// End-to-end through the real Artists grid: the search box renders above
    /// the grid, and typing a query that matches only one artist's name
    /// narrows the grid down to it; clearing it shows every artist again.
    #[test]
    fn artists_search_narrows_the_grid() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            artists_page(s, &p, cx, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![tv(1, "Bach"), tv(2, "Satie")];
        s.route = crate::Route::Artists;

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("artists-search").is_some(),
            "the search box renders above the grid"
        );
        // Match by rendered text, not slot key: a filtered grid re-indexes its
        // remaining cards from 0, so "artist-0" exists regardless of which
        // artist survived, only the text proves it's the right one. Skip
        // `exiting` elements: a removed card plays an exit transition and
        // stays in `elements()` with its stale text until it settles.
        let texts = |rt: &Runtime<State>| -> Vec<String> {
            rt.elements()
                .into_iter()
                .filter(|e| !e.exiting)
                .filter_map(|e| e.text)
                .collect()
        };
        assert!(
            texts(&rt).iter().any(|t| t == "Bach") && texts(&rt).iter().any(|t| t == "Satie"),
            "both artists show with no query"
        );

        rt.update(|st: &mut State, _tx: &mut Tx| st.artist_search = rune::TextEdit::new("bach"));
        rt.frame(0.0);
        assert!(texts(&rt).iter().any(|t| t == "Bach"), "the match stays");
        assert!(
            !texts(&rt).iter().any(|t| t == "Satie"),
            "the non-match is filtered out of the grid"
        );

        rt.update(|st: &mut State, _tx: &mut Tx| st.artist_search = rune::TextEdit::new(""));
        rt.frame(0.0);
        assert!(
            texts(&rt).iter().any(|t| t == "Bach") && texts(&rt).iter().any(|t| t == "Satie"),
            "clearing the query shows every artist again"
        );
    }

    /// The old app's artist card subtitle is "{n} albums · {n} tracks", not a
    /// bare track count: an artist spread across two albums shows both counts.
    #[test]
    fn artist_card_subtitle_shows_album_and_track_counts() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            artists_page(s, &p, cx, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        // Bach: 8 tracks across 2 albums. Satie: a lone 1-track, 1-album artist,
        // so the two artists' subtitles can't be confused for one another.
        s.library = vec![
            tv_album(1, "Bach", "Album One"),
            tv_album(2, "Bach", "Album One"),
            tv_album(3, "Bach", "Album One"),
            tv_album(4, "Bach", "Album One"),
            tv_album(5, "Bach", "Album Two"),
            tv_album(6, "Bach", "Album Two"),
            tv_album(7, "Bach", "Album Two"),
            tv_album(8, "Bach", "Album Two"),
            tv_album(9, "Satie", "Solo"),
        ];
        s.route = crate::Route::Artists;

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);
        let texts: Vec<String> = rt
            .elements()
            .into_iter()
            .filter(|e| !e.exiting)
            .filter_map(|e| e.text)
            .collect();
        assert!(
            texts.iter().any(|t| t == "2 albums · 8 tracks"),
            "Bach's card shows both its album and track counts, got: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "1 album · 1 track"),
            "Satie's card shows its own counts too (singular), got: {texts:?}"
        );
    }
}
