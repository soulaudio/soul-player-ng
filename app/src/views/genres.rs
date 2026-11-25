//! Genres browse: a responsive grid of card tiles (name + track count) that
//! drills into a genre's tracks. Genres carry no cover art, so each tile is a
//! flat card on the muted surface rather than the album-art square, matching
//! the original's genre card grid. Groups the flat library by genre; the backend's
//! `BrowseAxis::Genre` order already clusters a genre's tracks together, and the
//! importer's "Unknown Genre" is a real, keep-able bucket. Reuses albums'
//! `detail_track_row` so the detail list stays consistent with the other pages,
//! and `grid::responsive_grid` so the tile grid columns the same as Albums/Artists.
#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use rune::{Cursor, Cx, El, GrowthSpec, ScrollSpec, col, row, text};
use soul_player_host::TrackView;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::{detail_track_header, detail_track_row, fmt_total_dur};
use crate::views::detail_page::{HeroArt, detail_actions, detail_hero};
use crate::views::grid;
use crate::views::icons;
use crate::views::player::{group_remote_source, remote_badge_for};

/// One grouped genre: its display name, every track id (in library order) for
/// Play All / the detail list, and whether every one of its tracks streams from
/// the same remote host (`None` for a local or mixed-source genre, see
/// [`group_remote_source`]).
pub(crate) struct Genre {
    name: String,
    ids: Vec<u32>,
    remote_source: Option<String>,
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
            .as_deref()
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| t!("library.unknown").to_string());
        let i = *index.entry(name.clone()).or_insert_with(|| {
            genres.push(Genre {
                name,
                ids: Vec::new(),
                remote_source: None, // resolved below, once every member track is known
            });
            genres.len() - 1
        });
        genres[i].ids.push(t.id);
    }
    // See `group_albums`' matching second pass: a track id -> remote_source map
    // avoids an O(n²) `library.iter().find` per id.
    let remote_by_id: HashMap<u32, &Option<String>> =
        library.iter().map(|t| (t.id, &t.remote_source)).collect();
    for genre in &mut genres {
        genre.remote_source = group_remote_source(
            genre
                .ids
                .iter()
                .filter_map(|id| remote_by_id.get(id).copied()),
        );
    }
    genres
}

/// One genre tile: the genre name (bold) and its track count beneath, on a
/// card background (the original's genre card grid; genres carry no cover
/// art, so the tile is a flat card rather than an album-art square). Clicking
/// it opens the genre's detail page (its track list + Play All).
fn genre_tile(p: &Palette, slot: usize, g: &Genre, card_w: f32) -> El {
    let name = g.name.clone();
    let title = text(g.name.clone())
        .font(type_::ITEM_STRONG)
        .color(p.fg)
        .truncate();
    // A subtle cloud badge when every track in this genre streams from the same
    // remote host; nothing renders for a local/mixed genre, same unremarked
    // default as the album/artist cards and the track rows.
    let title_row: El = if let Some(host) = &g.remote_source {
        row()
            .key(format!("genre-tile-title-{slot}"))
            .gap(space::XS)
            .center_y()
            .min_w(0.0)
            .child(title.grow(1.0))
            .child(remote_badge_for(
                &format!("genre-tile-remote-{slot}"),
                host,
                p,
            ))
    } else {
        title
    };

    col()
        .key(format!("genre-tile-{slot}"))
        .role("button")
        .label(g.name.clone())
        .focusable(true)
        // Fixed to the computed column width (like the album/artist cards'
        // `size(card_w, ..)`), so every tile is exactly the same width and a
        // partial last row aligns left with the rows above rather than the last
        // tile stretching to fill the leftover space.
        .w(card_w)
        .fill(p.card)
        .radius(radius::MD)
        .padding(space::MD)
        .gap(space::XXS)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(title_row)
        .child(
            text(crate::views::library::tracks_label(g.ids.len()))
                .font(type_::CAPTION)
                .color(p.text_secondary),
        )
        .on_press(move |st: &mut State| {
            crate::navigate_to(
                st,
                crate::Route::Genre,
                None,
                None,
                Some(name.clone()),
                None,
            );
        })
}

/// The Genres page: a responsive grid of genre card tiles, scrolled vertically.
/// Mirrors the Albums/Artists grid (`grid::responsive_grid`), just without
/// cover art.
/// A genre tile's height (title + track-count + padding): its `virtual_grid` row
/// stride, since the tile has no cover.
const GENRE_TILE_H: f32 = 64.0;

/// A detail-page track row's estimated height, the stride the genre-detail track
/// list's `virtual_rows` windows by until rows are measured.
const DETAIL_ROW_EST_H: f32 = 40.0;

pub fn genres_page(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    // A per-page search box above the grid, matching the old app's Genres page
    // (and the Albums/Artists grids). Reuses `genre_search` (which resets on
    // nav, so it never leaks between this list and a genre's detail page).
    let q = s.genre_search.text.to_lowercase();
    // Grouping is MEMOIZED (see `State::grouped_genres`); search narrows it to a
    // list of INDICES, never cloning the genre list per frame.
    let grouped = s.grouped_genres();
    let visible: Vec<usize> = grouped
        .iter()
        .enumerate()
        .filter(|(_, g)| q.is_empty() || g.name.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect();
    // Read this page's own pane width (first frame has no prior layout → a
    // sensible desktop fallback), same rule the Albums/Artists grids use.
    let pane = cx.layout_size("genres").map(|[w, _]| w).unwrap_or(820.0);

    let search = crate::views::library::search_box(
        &s.genre_search,
        "genres-search",
        t!("genres.searchPlaceholder", count => grouped.len()).to_string(),
        p,
        |st: &mut State| &mut st.genre_search,
    );
    let search_bar = crate::views::library::page_search_bar("genres", search, None);

    // Virtualized grid of cover-less tiles (cover_aspect 0.0); only the on-screen
    // window is built. The closure captures the memoized grouping + visible indices.
    let grouped_cards = grouped.clone();
    let visible: Rc<[usize]> = visible.into();
    let count = visible.len();
    let vis_cards = visible.clone();
    let pal = *p;
    let grid = grid::virtual_grid(
        "genres-grid",
        pane,
        count,
        0.0, // no cover, the tile is just a title + count
        GENRE_TILE_H,
        Rc::new(move |_s: &State, pos: usize, card_w: f32| {
            genre_tile(&pal, pos, &grouped_cards[vis_cards[pos]], card_w)
        }),
    );
    col()
        .key("genres")
        .grow(1.0)
        .min_h(0.0)
        .child(search_bar)
        .child(
            col()
                .key("genres-scroll")
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .scroll_edge_fade(24.0)
                .grow(1.0)
                .min_h(0.0)
                .child(grid),
        )
}

/// A genre's detail page: a back link, the shared hero using a muted music-note
/// tile in place of artwork ("GENRE" eyebrow, name, "{n} tracks · {duration}",
/// Play All), then the genre's track list. The genre is `State::open_genre`,
/// re-derived from the current library; if it has vanished we just offer the way
/// back.
pub fn genre_detail(s: &State, p: &Palette, scale: f32) -> El {
    let grouped = s.grouped_genres();
    let Some(genre) = s
        .open_genre
        .as_deref()
        .and_then(|name| grouped.iter().find(|g| g.name == name))
    else {
        return col()
            .key("genre-detail")
            .grow(1.0)
            .min_h(0.0)
            .padding(space::PANE);
    };

    let total: f64 = genre
        .ids
        .iter()
        .filter_map(|id| s.track(*id))
        .map(|t| t.duration_secs)
        .sum();
    let meta = format!(
        "{} · {}",
        crate::views::library::tracks_label(genre.ids.len()),
        fmt_total_dur(total)
    );

    let hero = detail_hero(
        s,
        p,
        scale,
        "genre",
        HeroArt::Icon(&icons::MUSIC),
        t!("genre.eyebrow").to_uppercase(),
        genre.name.clone(),
        genre.remote_source.clone(),
        None,
        meta,
        Some(&icons::CLOCK),
        detail_actions("genre-play-all", genre.ids.clone(), p),
        None, // genres have no editable artwork (icon tile)
    );

    let q = s.genre_search.text.to_lowercase();
    // This genre's `(library indices, ids)`, filtered by `q`, MEMOIZED by (library
    // fingerprint, genre, query) so a big genre ("Rock" can be most of the library)
    // is not re-filtered and re-collected every frame (see
    // `State::visible_genre_tracks`); the virtual rows below index these directly.
    let (idxs, ids) = s.visible_genre_tracks(&genre.name, &genre.ids, &q);
    let search = crate::views::library::search_box(
        &s.genre_search,
        "genre-search",
        t!("genre.searchPlaceholder").to_string(),
        p,
        |st: &mut State| &mut st.genre_search,
    )
    .w_full();
    // Fixed top: the hero + search + (when there are rows) the column header. These
    // stay put while the track list scrolls below, which also lets the list be the
    // DIRECT child of its scroll viewport, as `virtual_rows` requires.
    let mut top = col()
        .key("genre-detail-top")
        .padding(space::PANE)
        .child(hero)
        .child(search);
    if !ids.is_empty() {
        top = top.child(detail_track_header(p, true, true));
    }
    // The scrolled, VIRTUALIZED track list: a huge genre lays out only the on-screen
    // window of rows, not all of them. The row closures capture the memoized library
    // `Rc` + the shown tracks' indices (O(1) lookups) so they clone no track data.
    let body: El = if ids.is_empty() {
        col()
            .key("genre-tracks")
            .px(space::PANE)
            .py(space::SM)
            .child(
                text(t!("genre.noMatches").to_string())
                    .font(type_::ITEM)
                    .color(p.text_secondary),
            )
    } else {
        let lib = s.library_rc();
        let pal = *p;
        let (now, playing) = (s.now_playing_id, s.playing);
        let count = idxs.len();
        let (build_lib, build_idxs, build_ids) = (lib.clone(), idxs.clone(), ids.clone());
        col()
            .key("genre-tracks")
            .role("list")
            .gap(space::XXS)
            .growth(GrowthSpec::default())
            .virtual_rows(
                count,
                DETAIL_ROW_EST_H,
                move |_s: &State, i: usize| Cow::Owned(format!("album-track-{i}")),
                move |_s: &State, i: usize| {
                    detail_track_row(
                        now,
                        playing,
                        &pal,
                        &build_ids,
                        i,
                        &build_lib[build_idxs[i]],
                        true, // a genre spans many artists
                        true, // and many albums
                    )
                },
            )
    };
    col()
        .key("genre-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(top)
        .child(
            col()
                .key("genre-tracks-scroll")
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .scroll_edge_fade(24.0)
                .grow(1.0)
                .min_h(0.0)
                .px(space::PANE)
                .child(body),
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
            genre: genre.map(|g| g.into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
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

    /// A genre's `remote_source` is the unanimous host across every one of its
    /// tracks; a mix of remote and local has no consensus and must show no
    /// badge.
    #[test]
    fn group_genres_resolves_remote_source_consensus() {
        let mut remote1 = tv(1, Some("Rock"));
        remote1.remote_source = Some("h".to_string());
        let mut remote2 = tv(2, Some("Rock"));
        remote2.remote_source = Some("h".to_string());
        let all_remote = group_genres(&[remote1, remote2]);
        assert_eq!(
            all_remote[0].remote_source,
            Some("h".to_string()),
            "every track shares the same host"
        );

        let mut remote = tv(3, Some("Jazz"));
        remote.remote_source = Some("h".to_string());
        let local = tv(4, Some("Jazz"));
        let mixed = group_genres(&[remote, local]);
        assert_eq!(
            mixed[0].remote_source, None,
            "a local track in the mix breaks consensus"
        );

        let all_local = group_genres(&[tv(5, Some("Jazz")), tv(6, Some("Jazz"))]);
        assert_eq!(all_local[0].remote_source, None, "all local -> no badge");
    }

    /// The Genres list renders as a card-tile grid (the original's look), not a
    /// plain text list: each genre is a focusable, labelled `genre-tile-*`
    /// element showing its name and "{n} tracks" count. The a11y sweep
    /// (`all_actionable_controls_are_labelled`) depends on every tile carrying
    /// a role + label, so this guards that directly too.
    #[test]
    fn genres_page_renders_a_card_tile_grid() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            genres_page(s, &p, cx)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![
            tv(1, Some("Rock")),
            tv(2, Some("Rock")),
            tv(3, Some("Jazz")),
        ];
        s.route = crate::Route::Genres;

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let tiles: Vec<_> = rt
            .elements()
            .into_iter()
            .filter(|e| {
                e.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("genre-tile-"))
            })
            .collect();
        // Two genres → two tiles, each focusable with a button role and an
        // accessible name (its genre name), exactly what the a11y sweep checks.
        let rock = tiles
            .iter()
            .find(|e| e.label.as_deref() == Some("Rock"))
            .expect("a focusable, labelled Rock tile renders");
        assert!(rock.focusable, "the tile is keyboard-focusable");
        assert_eq!(rock.role, "button", "the tile carries an interactive role");
        assert!(
            tiles.iter().any(|e| e.label.as_deref() == Some("Jazz")),
            "a Jazz tile renders too"
        );

        // The name (bold title) and the "{n} tracks" count both render as text.
        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(texts.iter().any(|t| t == "Rock"), "the genre name renders");
        assert!(
            texts.iter().any(|t| t == "2 tracks"),
            "Rock's track count renders, got: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "1 track"),
            "Jazz's track count renders too (singular), got: {texts:?}"
        );
    }

    /// The Genres list search filters the tiles by name (empty query = all),
    /// matching the old app's Genres page and the Albums/Artists grids.
    #[test]
    fn genres_search_narrows_the_tiles() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            genres_page(s, &p, cx)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![
            tv(1, Some("Rock")),
            tv(2, Some("Jazz")),
            tv(3, Some("Electronic")),
        ];
        s.route = crate::Route::Genres;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("genres-search").is_some(),
            "the search box renders above the grid"
        );

        // Skip `exiting` tiles: a filtered-out tile plays a removal animation and
        // lingers in `elements()` with its stale label until it settles.
        let tile_labels = |rt: &Runtime<State>| -> Vec<String> {
            rt.elements()
                .into_iter()
                .filter(|e| !e.exiting)
                .filter(|e| {
                    e.key
                        .as_deref()
                        .is_some_and(|k| k.starts_with("genre-tile-"))
                })
                .filter_map(|e| e.label)
                .collect()
        };
        assert_eq!(tile_labels(&rt).len(), 3, "all genres show with no query");

        rt.update(|st: &mut State, _tx: &mut rune::Tx| {
            st.genre_search = rune::TextEdit::new("jazz")
        });
        rt.frame(0.0);
        let labels = tile_labels(&rt);
        assert!(
            labels.contains(&"Jazz".to_string()),
            "the matching genre stays, got: {labels:?}"
        );
        assert!(
            !labels.contains(&"Rock".to_string()) && !labels.contains(&"Electronic".to_string()),
            "non-matching genres are filtered out, got: {labels:?}"
        );
    }

    /// The genre detail's filtered track list is MEMOIZED by (library fingerprint,
    /// genre, query): a repeat call at the same query returns the SAME allocation (a
    /// cache hit, so the per-frame filter + collect over a big genre does not rerun),
    /// and a changed query rebuilds and re-filters. Guards the per-frame O(genre)
    /// allocation fix for the detail page.
    #[test]
    fn visible_genre_tracks_is_memoized_per_query() {
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![
            tv(1, Some("Rock")),
            tv(2, Some("Rock")),
            tv(3, Some("Jazz")),
        ];
        let grouped = s.grouped_genres();
        let ids = grouped
            .iter()
            .find(|g| g.name == "Rock")
            .expect("a Rock genre")
            .ids
            .clone();

        // Same (genre, query) twice: the returned id list is the SAME allocation, so
        // the filter + collect ran once, not once per call (per frame).
        let (_i1, a) = s.visible_genre_tracks("Rock", &ids, "");
        let (_i2, b) = s.visible_genre_tracks("Rock", &ids, "");
        assert!(
            Rc::ptr_eq(&a, &b),
            "a repeat call at the same query must be a cache hit, not a rebuild"
        );
        assert_eq!(&*a, &[1, 2], "both Rock tracks show with an empty query");

        // A changed query invalidates the cache and re-filters: "t1" matches only
        // track 1's title ("T1"), not "T2".
        let (_i3, c) = s.visible_genre_tracks("Rock", &ids, "t1");
        assert!(
            !Rc::ptr_eq(&a, &c),
            "a changed query must invalidate the cache"
        );
        assert_eq!(&*c, &[1], "only the track matching the query survives");
    }
}
