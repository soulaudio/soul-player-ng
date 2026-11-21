//! Genres browse: a responsive grid of card tiles (name + track count) that
//! drills into a genre's tracks. Genres carry no cover art, so each tile is a
//! flat card on the muted surface rather than the album-art square, matching
//! the original's genre card grid. Groups the flat library by genre; the backend's
//! `BrowseAxis::Genre` order already clusters a genre's tracks together, and the
//! importer's "Unknown Genre" is a real, keep-able bucket. Reuses albums'
//! `detail_track_row` so the detail list stays consistent with the other pages,
//! and `grid::responsive_grid` so the tile grid columns the same as Albums/Artists.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use rune::{Cursor, Cx, El, ScrollSpec, col, el, row, text, text_input};
use soul_player_host::TrackView;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::{detail_track_row, fmt_total_dur};
use crate::views::detail_page::{HeroArt, detail_hero, play_all_pill};
use crate::views::grid;
use crate::views::icons;
use crate::views::player::{group_remote_source, remote_badge_for};

/// One grouped genre: its display name, every track id (in library order) for
/// Play All / the detail list, and whether every one of its tracks streams from
/// the same remote host (`None` for a local or mixed-source genre — see
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
            .clone()
            .filter(|g| !g.is_empty())
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
        // A flex item like the album/artist cards: wrap at MIN_CARD (the same
        // threshold `responsive_grid` counts columns by), then grow to fill the
        // row, capped so a partial last row stays aligned with the rows above.
        .min_w(grid::MIN_CARD)
        .max_w(card_w + grid::FILL_SLOP)
        .grow(1.0)
        .fill(p.card)
        .radius(radius::MD)
        .padding(space::MD)
        .gap(space::XXS)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(title_row)
        .child(
            text(t!("genre.trackCount", count => g.ids.len()).to_string())
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
pub fn genres_page(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let genres = group_genres(&s.library);
    // Read this page's own pane width (first frame has no prior layout → a
    // sensible desktop fallback), same rule the Albums/Artists grids use.
    let pane = cx.layout_size("genres").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid::responsive_grid(pane, grid::MIN_CARD, space::GUTTER).1;

    let mut tiles = row()
        .key("genres-grid")
        .role("grid")
        .flex_wrap()
        .gaps(space::MD, space::GUTTER)
        .padding(space::PANE);
    for (slot, g) in genres.iter().enumerate() {
        tiles = tiles.child(genre_tile(p, slot, g, card_w));
    }
    col().key("genres").grow(1.0).min_h(0.0).child(
        // Scroll on the growing wrapper, not the grid itself, see `albums_page`: a
        // flex-wrap grid that is itself the growing scroll element stretches its
        // wrapped rows down the viewport when they don't fill it. Natural-height
        // grid, growing scroll viewport around it.
        col()
            .key("genres-scroll")
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .scroll_edge_fade(24.0)
            .grow(1.0)
            .min_h(0.0)
            .child(tiles),
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
        .on_press(crate::go_back)
}

/// A genre's detail page: a back link, the shared hero using a muted music-note
/// tile in place of artwork ("GENRE" eyebrow, name, "{n} tracks · {duration}",
/// Play All), then the genre's track list. The genre is `State::open_genre`,
/// re-derived from the current library; if it has vanished we just offer the way
/// back.
pub fn genre_detail(s: &State, p: &Palette, scale: f32) -> El {
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

    let total: f64 = genre
        .ids
        .iter()
        .filter_map(|id| s.library.iter().find(|t| t.id == *id))
        .map(|t| t.duration_secs)
        .sum();
    let meta = t!(
        "detail.tracksAndDuration",
        count => genre.ids.len(),
        dur => fmt_total_dur(total)
    )
    .to_string();

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
        play_all_pill("genre-play-all", genre.ids.clone(), p),
    );

    let q = s.genre_search.text.to_lowercase();
    let shown: Vec<u32> = genre
        .ids
        .iter()
        .copied()
        .filter(|id| {
            q.is_empty()
                || s.library
                    .iter()
                    .find(|t| t.id == *id)
                    .is_some_and(|t| crate::views::library::track_matches(t, &q))
        })
        .collect();
    let search = text_input(&s.genre_search, type_::ITEM)
        .key("genre-search")
        .single_line()
        .label(t!("genre.searchPlaceholder").to_string())
        .w_full()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .color(p.fg)
        .on_edit(|st: &mut State| &mut st.genre_search);
    let mut list = col().key("genre-tracks").role("list").gap(space::XXS);
    if shown.is_empty() {
        list = list.child(
            text(t!("genre.noMatches").to_string())
                .font(type_::ITEM)
                .color(p.text_secondary)
                .px(space::MD)
                .py(space::SM),
        );
    } else {
        for (i, &id) in shown.iter().enumerate() {
            if let Some(t) = s.library.iter().find(|t| t.id == id) {
                list = list.child(detail_track_row(
                    s.now_playing_id,
                    s.playing,
                    p,
                    &shown,
                    i,
                    t,
                ));
            }
        }
    }

    col()
        .key("genre-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(row_top(p))
        .child(
            col()
                .key("genre-detail-content")
                .padding(space::PANE)
                .child(hero)
                .child(search)
                .child(list)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .scroll_edge_fade(24.0)
                .grow(1.0)
                .min_h(0.0),
        )
}

/// The back-link bar above the genre detail content.
fn row_top(p: &Palette) -> El {
    row()
        .key("genre-detail-top")
        .px(space::PANE)
        .pt(space::SM)
        .child(back_button(p))
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
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
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
            texts.iter().any(|t| t == "1 tracks"),
            "Jazz's track count renders too, got: {texts:?}"
        );
    }
}
