//! Albums grid + an album's detail page, mirroring the old app's
//! AlbumsPage/MediaCard and AlbumPage. Each card is a square cover + title +
//! an "Artist · year" subtitle (the old app's "artist • year"), the year read
//! from `State.album_year`'s selected date with a fallback to the other one;
//! the hover play button plays/pauses the album, the card body opens its
//! detail page.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::rc::Rc;

use rune::{
    Align, Cursor, Cx, El, PointerPress, PopupItem, PopupMenu, ScrollSpec, Tx, col, el, icon, row,
    text,
};
use soul_player_host::{Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::detail_page::{ArtworkTarget, HeroArt, detail_actions, detail_hero};
use crate::views::grid;
use crate::views::library::fmt_dur;
use crate::views::media_card::{CardSpec, media_card};
use crate::views::player::{group_remote_source, remote_badge};

/// One grouped album (grouped by `TrackView::album_key`, so two same-title
/// albums stay apart and CD1/CD2 of one release join): its identity, display
/// name, its artist (the album artist, falling back to the first track's
/// artist), a track id to draw the cover from, every track id (in `(disc,
/// track)` order) to play on click, the album's
/// original/release dates (packed `(year<<9)|(month<<5)|day`, `0` = unknown;
/// see [`album_year`]), and whether every one of its tracks streams from the
/// same remote host (`None` for a local or mixed-source album, see
/// [`group_remote_source`]).
#[derive(Clone)]
pub(crate) struct Album {
    /// The album's identity (`TrackView::album_key`).
    pub(crate) key: u32,
    pub(crate) name: String,
    /// The album artist, falling back to the first track's artist.
    pub(crate) artist: String,
    /// The album's disc count (`>= 1`).
    pub(crate) disc_count: u16,
    pub(crate) cover_id: u32,
    pub(crate) ids: Vec<u32>,
    pub(crate) original_date: u32,
    pub(crate) release_date: u32,
    pub(crate) remote_source: Option<String>,
    /// The album's dynamic-range indicator: the mean of its analyzed tracks' DR (the TT /
    /// dr.loudness-war convention), or `None` until at least one member track is analyzed.
    pub(crate) dr: Option<f32>,
    /// The distinct genres across the album's tracks (sorted, deduped). Lets the
    /// Albums page's genre Filters narrow the MEMOIZED grouping instead of
    /// re-grouping a filtered library subset every frame.
    pub(crate) genres: Vec<String>,
}

impl Album {
    /// The navigation reference that opens this album's detail page.
    pub(crate) fn open_ref(&self) -> crate::AlbumRef {
        crate::AlbumRef {
            key: self.key,
            name: self.name.clone(),
        }
    }
}

/// The open album: same key AND title, else (after a rescan re-numbered ids) the
/// first album with that title, else `None`.
pub(crate) fn resolve_album<'a>(albums: &'a [Album], r: &crate::AlbumRef) -> Option<&'a Album> {
    albums
        .iter()
        .find(|a| a.key == r.key && a.name == r.name)
        .or_else(|| albums.iter().find(|a| a.name == r.name))
}

/// An album's DR indicator: the mean of its member tracks' DR over those analyzed so far (the
/// TT / dr.loudness-war album-DR convention), or `None` when none are analyzed yet. `drs`
/// yields the DR of each already-analyzed member track.
pub(crate) fn album_dr<I: IntoIterator<Item = f32>>(drs: I) -> Option<f32> {
    let (sum, n) = drs
        .into_iter()
        .fold((0.0f32, 0u32), |(s, n), d| (s + d, n + 1));
    if n == 0 { None } else { Some(sum / n as f32) }
}

/// Group the flat library into albums by identity (`album_key`), preserving
/// first-seen (library) order so the grid follows the baked sort; within an
/// album the ids are in `(disc, track)` order. A track with no album title is
/// named "Unknown".
pub(crate) fn group_albums(library: &[TrackView]) -> Vec<Album> {
    let mut albums: Vec<Album> = Vec::new();
    let mut index: HashMap<u32, usize> = HashMap::new();
    for t in library {
        let i = *index.entry(t.album_key).or_insert_with(|| {
            let name = t
                .album
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| t!("library.unknown").to_string());
            let artist = t
                .album_artist
                .as_deref()
                .filter(|a| !a.is_empty())
                .or(t.artist.as_deref())
                .unwrap_or_default()
                .to_string();
            albums.push(Album {
                key: t.album_key,
                name,
                artist,
                disc_count: t.disc_count.max(1),
                cover_id: t.id,
                ids: Vec::new(),
                original_date: t.original_date,
                release_date: t.release_date,
                remote_source: None, // resolved below, once every member track is known
                dr: None,            // resolved below from the member tracks' loudness
                genres: Vec::new(),  // resolved below from the member tracks' genres
            });
            albums.len() - 1
        });
        albums[i].ids.push(t.id);
    }
    // Within an album, play and list order is (disc, track) whatever the library's
    // sort axis; the grid itself keeps first-seen (baked) album order.
    let view_by_id: HashMap<u32, &TrackView> = library.iter().map(|t| (t.id, t)).collect();
    for album in &mut albums {
        album.ids.sort_by_key(|id| {
            view_by_id.get(id).map_or((u16::MAX, u16::MAX), |t| {
                soul_player_host::disc_track_key(t)
            })
        });
    }
    // A second pass: each album's remote-source consensus needs every one of its
    // member tracks, which aren't all known until the loop above finishes. A
    // `track id -> remote_source` map avoids an O(n²) `library.iter().find` per id.
    let remote_by_id: HashMap<u32, &Option<String>> =
        library.iter().map(|t| (t.id, &t.remote_source)).collect();
    for album in &mut albums {
        album.remote_source = group_remote_source(
            album
                .ids
                .iter()
                .filter_map(|id| remote_by_id.get(id).copied()),
        );
    }
    // Album DR = mean of the member tracks' DR over those analyzed so far. Same `id -> dr`
    // map rationale as the remote-source pass (avoid an O(n²) find per member id).
    let dr_by_id: HashMap<u32, f32> = library
        .iter()
        .filter_map(|t| {
            let d = t.loudness.as_ref()?.dr;
            d.is_finite().then_some((t.id, d))
        })
        .collect();
    for album in &mut albums {
        album.dr = album_dr(album.ids.iter().filter_map(|id| dr_by_id.get(id).copied()));
    }
    // Album genres = the distinct genres over its member tracks. Same `id -> genre`
    // map rationale as the passes above (avoid an O(n²) find per member id).
    let genre_by_id: HashMap<u32, &str> = library
        .iter()
        .filter_map(|t| {
            t.genre
                .as_deref()
                .filter(|g| !g.is_empty())
                .map(|g| (t.id, g))
        })
        .collect();
    for album in &mut albums {
        let mut gs: Vec<String> = album
            .ids
            .iter()
            .filter_map(|id| genre_by_id.get(id).map(|g| g.to_string()))
            .collect();
        gs.sort_unstable();
        gs.dedup();
        album.genres = gs;
    }
    albums
}

/// Test-only: a stable album key derived from an album NAME (FNV-1a; `0` for
/// `None`), so hand-built `TrackView` fixtures that share a title share one album,
/// the grouping those fixtures were written for. Real views carry the catalog's
/// compound album id instead.
#[cfg(test)]
pub(crate) fn test_album_key(album: Option<&str>) -> u32 {
    album.map_or(0, |a| {
        a.bytes().fold(0x811c_9dc5u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        })
    })
}

/// Narrow `albums` to those whose name or artist contains `query`, a
/// case-insensitive substring match; `query` must already be lowercased (the
/// caller lowercases the search box's text once rather than per-album). An
/// empty query keeps every album, same convention as the genre-detail search.
#[cfg(test)]
pub(crate) fn filter_albums(albums: Vec<Album>, query: &str) -> Vec<Album> {
    if query.is_empty() {
        return albums;
    }
    albums
        .into_iter()
        .filter(|a| {
            a.name.to_lowercase().contains(query) || a.artist.to_lowercase().contains(query)
        })
        .collect()
}

/// The album's display year for the chosen source: the selected date, falling
/// back to the other when the selected one is unknown; `None` if both are
/// unknown. Packed `u32` → year via `>> 9`; `0` = unknown.
pub(crate) fn album_year(a: &Album, src: crate::config::AlbumYearSource) -> Option<u16> {
    use crate::config::AlbumYearSource::Original;
    let (first, second) = if src == Original {
        (a.original_date, a.release_date)
    } else {
        (a.release_date, a.original_date)
    };
    let d = if first != 0 { first } else { second };
    (d != 0).then_some((d >> 9) as u16)
}

/// The card/detail subtitle: "Artist · year" when both are known, the artist
/// alone when the year is unknown, the year alone when the artist is empty, or
/// `None` when neither is available (the card renders no second line).
fn album_subtitle(a: &Album, src: crate::config::AlbumYearSource) -> Option<String> {
    match (a.artist.is_empty(), album_year(a, src)) {
        (true, Some(y)) => Some(y.to_string()),
        (true, None) => None,
        (false, Some(y)) => Some(format!("{} · {y}", a.artist)),
        (false, None) => Some(a.artist.clone()),
    }
}

/// Sum of track seconds as a coarse human total, "1 h 23 min" or "45 min"
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

/// The albums of a single artist by a library-wide name filter: fold the matching
/// tracks by album (first-seen order), then sort via [`discography_of`]. This is the
/// O(library) name-scan the artist page used to run every frame; production now
/// resolves the artist's OWN track ids (`group_artists`) through `discography_of`
/// directly (O(artist)). Kept as the equivalence oracle those tests check against.
#[cfg(test)]
pub(crate) fn albums_of_artist(
    library: &[TrackView],
    artist: &str,
    src: crate::config::AlbumYearSource,
) -> Vec<Album> {
    let subset: Vec<TrackView> = library
        .iter()
        .filter(|t| {
            let a = t
                .artist
                .as_deref()
                .filter(|a| !a.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| t!("library.unknown").to_string());
            a == artist
        })
        .cloned()
        .collect();
    discography_of(&subset, src)
}

/// Fold a set of tracks already filtered to a single artist into that artist's
/// Discography: group by album (first-seen order), then re-sort chronologically by
/// the selected date source (unknown `0` dates last; the sort is stable, so
/// equal/unknown years keep their first-seen order). Shared by [`albums_of_artist`]
/// (library-wide name filter) and the artist detail page, which feeds the artist's
/// own resolved tracks (O(artist), not an O(library) re-scan).
pub(crate) fn discography_of(
    tracks: &[TrackView],
    src: crate::config::AlbumYearSource,
) -> Vec<Album> {
    let mut albums = group_albums(tracks);
    let key = |a: &Album| -> u32 {
        let (first, second) = if src == crate::config::AlbumYearSource::Original {
            (a.original_date, a.release_date)
        } else {
            (a.release_date, a.original_date)
        };
        let d = if first != 0 { first } else { second };
        if d == 0 { u32::MAX } else { d }
    };
    albums.sort_by_key(key);
    albums
}

/// One album card: a square cover with a hover play-overlay, the title, and the
/// artist. Clicking the body opens the album's detail page; the overlay plays.
fn album_card(s: &State, p: &Palette, slot: usize, a: &Album, card_w: f32, scale: f32) -> El {
    // The right-click menu's play context + artwork target (cloned so the outer
    // handler stays `Fn`, like the track row's menu above).
    let menu_ids = a.ids.clone();
    let menu_name = a.name.clone();
    // What the body click opens: the album's identity, not just its title.
    let open = a.open_ref();
    media_card(
        s,
        p,
        CardSpec {
            key: format!("album-{slot}"),
            label: a.name.clone(),
            title: a.name.clone(),
            subtitle: album_subtitle(a, s.album_year),
            cover_id: a.cover_id,
            cover_override: s.covers.album_cover_path(&a.name),
            circular: false,
            play_ids: a.ids.clone(),
            remote_source: a.remote_source.clone(),
            // The DR badge follows the same loudness-columns toggle the Tracks
            // page uses (Settings -> Appearance), so turning loudness off hides
            // it on the album grid cards too.
            dr: s.show_loudness_columns.then_some(a.dr).flatten(),
        },
        card_w,
        scale,
        move |st: &mut State| {
            crate::navigate_to(
                st,
                crate::Route::Album,
                Some(open.clone()),
                None,
                None,
                None,
            );
        },
    )
    // Right-click the grid card for the old app's album actions (Play the album,
    // Edit Artwork). Mirrors the track row's `on_secondary_press` popup.
    .on_secondary_press(move |_st: &mut State, press: PointerPress, tx: &mut Tx| {
        let play_ids = menu_ids.clone();
        let art_name = menu_name.clone();
        tx.popup_menu(
            PopupMenu::new()
                .item(PopupItem::new("play", t!("player.play").to_string()))
                .item(PopupItem::new(
                    "add-to-playlist",
                    t!("playlists.addTo").to_string(),
                ))
                .item(PopupItem::new(
                    "edit-artwork",
                    t!("artwork.edit").to_string(),
                )),
            press,
            move |st: &mut State, picked, _tx: &mut Tx| match picked {
                Some("play") => st.host.send(Command::PlayContext {
                    ids: play_ids.clone(),
                    start: 0,
                }),
                Some("add-to-playlist") => crate::open_add_to_playlist_many(st, play_ids.clone()),
                Some("edit-artwork") => {
                    st.artwork_edit = Some(ArtworkTarget::Album(art_name.clone()));
                }
                _ => {}
            },
        );
    })
}

/// An album card's non-cover height (title + "Artist · year" subtitle): the
/// `virtual_grid` row stride is a `card_w`-square cover plus this.
const ALBUM_CAPTION_H: f32 = 48.0;

/// The Albums page: an always-visible search box above a responsive grid of
/// album cards, scrolled vertically. Mirrors the original soul-player's
/// per-page search (and the genre-detail page's own search box, which this
/// copies the styling of).
pub fn albums_page(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let q = s.album_search.text.to_lowercase();
    // The grouping is MEMOIZED (recomputed only when the library changes, not every
    // frame, see `State::grouped_albums`, which turned a ~2 s/frame cost at 1M
    // tracks into a cache hit). Genre Filters + search then narrow it to a list of
    // INDICES into that shared grouping, never cloning the (up to hundreds of
    // thousands of) albums per frame. An album matches a genre filter when any of
    // its tracks is in a selected genre (its `genres` set, resolved at group time).
    let grouped = s.grouped_albums();
    let genre_filter = &s.album_genre_filter;
    let visible: Vec<usize> = grouped
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            (genre_filter.is_empty() || a.genres.iter().any(|g| genre_filter.contains(g)))
                && (q.is_empty()
                    || a.name.to_lowercase().contains(&q)
                    || a.artist.to_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect();
    // Responsive grid (the old app's `grid-cols-2 sm:3 md:4 lg:5 xl:6 2xl:8`). Read
    // the page's laid-out width; the first frame has no prior layout, so fall back
    // to a sensible desktop width.
    let pane = cx.layout_size("albums").map(|[w, _]| w).unwrap_or(820.0);

    let search = crate::views::library::search_box(
        &s.album_search,
        "albums-search",
        t!("albums.searchPlaceholder", count => grouped.len()).to_string(),
        p,
        |st: &mut State| &mut st.album_search,
    );
    // A Filters button that toggles the genre-chip row (old app's "Filters").
    // Lit while open or while a genre filter is active.
    let filters_lit = s.albums_filters_open || !s.album_genre_filter.is_empty();
    let filters_btn = row()
        .key("albums-filters-toggle")
        .role("button")
        .label(t!("albums.filters").to_string())
        .focusable(true)
        .center_y()
        .gap(space::XS)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if filters_lit {
            p.accent.alpha(0.2)
        } else {
            p.muted
        })
        .hover(p.fg.alpha(fx::HOVER))
        .child(
            icon(&crate::views::icons::FILTER, 14.0).color(if filters_lit {
                p.accent
            } else {
                p.text_secondary
            }),
        )
        .child(
            text(t!("albums.filters").to_string())
                .font(type_::ITEM)
                .color(if filters_lit { p.accent } else { p.fg }),
        )
        .on_press(|st: &mut State| st.albums_filters_open = !st.albums_filters_open);
    let search_bar = crate::views::library::page_search_bar("albums", search, Some(filters_btn));
    // The expandable genre-chip row (one toggle chip per library genre). Built while
    // Filters is open OR still fading out after a toggle-off (reveal progress > 0),
    // so the row stays present to fade rather than vanishing instantly. `open` keeps
    // it fully shown + interactive the instant it opens (the poll animates only the
    // fade-OUT; a poll-less test still sees it at full strength).
    let filters_open = s.albums_filters_open;
    let chips_progress = s.albums_filters_reveal;
    let chips = (filters_open || chips_progress > 0.0).then(|| {
        let mut genres: Vec<&str> = s
            .library
            .iter()
            .filter_map(|t| t.genre.as_deref())
            .filter(|g| !g.is_empty())
            .collect();
        genres.sort_unstable();
        genres.dedup();
        let mut chip_row = row()
            .key("albums-genre-chips")
            .px(space::PANE)
            .pt(space::SM)
            .gaps(space::SM, space::SM)
            .flex_wrap();
        for (i, g) in genres.into_iter().enumerate() {
            let selected = s.album_genre_filter.contains(g);
            let g_owned = g.to_string();
            chip_row = chip_row.child(
                row()
                    .key(format!("albums-genre-chip-{i}"))
                    .role("button")
                    .label(g.to_string())
                    .focusable(true)
                    .px(space::MD)
                    .py(space::XS)
                    .radius(radius::LG)
                    .cursor(Cursor::Pointer)
                    .fill(if selected {
                        p.accent.alpha(0.3)
                    } else {
                        p.muted
                    })
                    .hover(p.fg.alpha(fx::HOVER))
                    .child(text(g.to_string()).font(type_::ITEM).color(if selected {
                        p.accent
                    } else {
                        p.fg
                    }))
                    .on_press(move |st: &mut State| {
                        if st.album_genre_filter.contains(&g_owned) {
                            st.album_genre_filter.remove(&g_owned);
                        } else {
                            st.album_genre_filter.insert(g_owned.clone());
                        }
                    }),
            );
        }
        chip_row
    });

    // Virtualized grid: chunk the visible albums into rows of cards; only the
    // on-screen window is ever built. The build closure captures the memoized
    // grouping + the visible index list by `Rc`, so it borrows nothing from `s`.
    let grouped_cards = grouped.clone();
    let visible: Rc<[usize]> = visible.into();
    let count = visible.len();
    let vis_cards = visible.clone();
    let pal = *p;
    let grid = grid::virtual_grid(
        "albums-grid",
        pane,
        count,
        1.0, // square cover
        ALBUM_CAPTION_H,
        Rc::new(move |s: &State, pos: usize, card_w: f32| {
            album_card(s, &pal, pos, &grouped_cards[vis_cards[pos]], card_w, scale)
        }),
    );
    let mut page = col().key("albums").grow(1.0).min_h(0.0).child(search_bar);
    if let Some(chip_row) = chips {
        // Fully shown while open; the shared `reveal` fade+collapse (same as the
        // sidebar) drives only the fade-OUT (open=false, progress 1->0): fade at full
        // height first, then collapse. Measure the row's natural height so the
        // collapse scales it; the first frame (no prior layout) hugs the content.
        let (op, hf) = if filters_open {
            (1.0, 1.0)
        } else {
            (
                crate::views::reveal::opacity(chips_progress),
                crate::views::reveal::height_factor(chips_progress),
            )
        };
        let natural_h = cx
            .layout_size("albums-genre-chips")
            .map(|[_, h]| h)
            .filter(|h| *h > 0.0);
        let mut wrapper = el()
            .key("albums-genre-chips-reveal")
            .w_full()
            .clip_rounded(radius::SM)
            .opacity(op)
            .child(chip_row);
        if let Some(h) = natural_h {
            wrapper = wrapper.h(h * hf);
        }
        page = page.child(wrapper);
    }
    page.child(
        // Scroll on the growing wrapper, NOT the grid itself. A flex-wrap grid that is
        // the growing scroll element has flexbox stretch its wrapped lines to fill the
        // height, spreading the rows apart whenever the library doesn't fill the window
        // (worse the more you zoom, fewer rows fit). A natural-height grid inside a
        // growing scroll viewport packs rows to the top (extra space falls below the
        // last row) and still scrolls once the content is taller than the viewport.
        col()
            .key("albums-scroll")
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .scroll_edge_fade(24.0)
            .grow(1.0)
            .min_h(0.0)
            .child(grid),
    )
}

/// One row on the album detail page: the track number (or a ⏸/▶ state glyph when
/// it's the current track), title and duration. Clicking plays the album from
/// that track, or toggles pause/resume when it is already the one playing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn detail_track_row(
    now_playing: Option<u32>,
    is_playing: bool,
    p: &Palette,
    ids: &[u32],
    i: usize,
    // The row number to show; `None` numbers by position `i + 1`.
    number: Option<u16>,
    t: &TrackView,
    // Which of the ARTIST / ALBUM columns to show. The old app always shows both;
    // we show them only where they add information: genre and playlist detail
    // (tracks span many artists/albums) show both, artist detail shows ALBUM
    // (one artist, many albums), album detail shows neither (both are constant).
    show_artist: bool,
    show_album: bool,
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
        .center_y()
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
                number.map_or_else(|| format!("{}", i + 1), |n| n.to_string())
            })
            .key(format!("album-track-{i}-no"))
            .font(type_::ITEM)
            .color(if playing { p.accent } else { p.text_secondary })
            .align(Align::Center)
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
            .grow(1.0)
            .min_w(0.0),
        );
    // ARTIST / ALBUM columns (the old app's TrackList), each a flexible column
    // sharing the row with the title, shown only where it adds information.
    if show_artist {
        r = r.child(
            text(t.artist.as_deref().unwrap_or_default())
                .font(type_::ITEM)
                .color(p.text_secondary)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        );
    }
    if show_album {
        r = r.child(
            text(t.album.as_deref().unwrap_or_default())
                .font(type_::ITEM)
                .color(p.text_secondary)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        );
    }
    // A subtle cloud badge for a track streaming from a remote source;
    // nothing renders for a local track, the unremarked default.
    if let Some(host) = &t.remote_source {
        r = r.child(remote_badge(t.id, host, p));
    }
    // Codec format badge (FLAC / MP3 / DSF ...), the same color-coded pill the
    // Tracks table uses, matching the old app's FORMAT column. Nothing renders
    // when the format is unknown.
    if let Some(fmt) = &t.format {
        r = r.child(crate::views::library::format_badge(fmt, p));
    }
    // Right-click opens the same track actions as a library row (the add-to-
    // playlist item opens the themed `views::add_to_playlist` overlay), so a
    // track is reachable from every detail page (album/artist/genre/playlist),
    // not just the library. `menu_ids` clones the play context for the menu's
    // "Play" action; the row's own `on_press` still owns `ids`.
    let menu_ids = ids.clone();
    r.child(
        text(fmt_dur(t.duration_secs))
            .font(type_::ITEM)
            .color(p.text_secondary)
            .w(56.0),
    )
    .on_secondary_press(move |_st: &mut State, press: PointerPress, tx: &mut Tx| {
        // Clone per invocation so this outer handler stays `Fn` (the inner
        // `move` callback would otherwise move `menu_ids` out of it).
        let play_ids = menu_ids.clone();
        tx.popup_menu(
            PopupMenu::new()
                .item(PopupItem::new("play", t!("player.play").to_string()))
                .item(PopupItem::new(
                    "play-next",
                    t!("library.playNext").to_string(),
                ))
                .item(PopupItem::new(
                    "enqueue",
                    t!("library.addToQueue").to_string(),
                ))
                .item(PopupItem::new(
                    "add-to-playlist",
                    t!("playlists.addTo").to_string(),
                )),
            press,
            move |st: &mut State, picked, _tx: &mut Tx| match picked {
                Some("play") => st.host.send(Command::PlayContext {
                    ids: play_ids.clone(),
                    start,
                }),
                Some("play-next") => st.host.send(Command::PlayNext(tid)),
                Some("enqueue") => st.host.send(Command::Enqueue(tid)),
                Some("add-to-playlist") => crate::open_add_to_playlist(st, tid),
                _ => {}
            },
        );
    })
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

/// Uppercase column-header labels over the album-detail track table, mirroring
/// [`detail_track_row`]'s columns exactly: a blank `#`-width spacer, TITLE
/// (grow), FORMAT (over the codec-badge area) and DURATION. `detail_track_row`
/// doesn't gate any of these by pane width, so neither does the header.
pub(crate) fn detail_track_header(p: &Palette, show_artist: bool, show_album: bool) -> El {
    let col_head = |label: String| {
        text(label)
            .font(type_::CAPTION)
            .color(p.text_secondary)
            .grow(1.0)
            .truncate()
            .min_w(0.0)
    };
    let mut h = row()
        .key("album-tracks-header")
        .gap(space::MD)
        .center_y()
        .px(space::MD)
        .py(space::XS)
        .child(el().key("album-tracks-header-index").w(28.0))
        .child(col_head(t!("library.colTitle").to_uppercase()));
    if show_artist {
        h = h.child(col_head(t!("library.colArtist").to_uppercase()));
    }
    if show_album {
        h = h.child(col_head(t!("library.colAlbum").to_uppercase()));
    }
    h.child(
        el().key("album-tracks-header-format").w(52.0).child(
            text(t!("library.colFormat").to_uppercase())
                .font(type_::CAPTION)
                .color(p.text_secondary),
        ),
    )
    .child(
        text(t!("library.colDuration").to_uppercase())
            .font(type_::CAPTION)
            .color(p.text_secondary)
            .w(56.0),
    )
}

/// A multi-disc album's disc header row: "Disc 2 · Liyue" (subtitle when present)
/// and the disc's total duration, aligned with the track rows' insets.
fn disc_header(d: &soul_player_host::DiscSection, p: &Palette) -> El {
    let title = match &d.subtitle {
        Some(sub) => t!("album.discWithSubtitle", n => d.disc_no, subtitle => sub).to_string(),
        None => t!("album.disc", n => d.disc_no).to_string(),
    };
    row()
        .key(format!("album-disc-{}", d.disc_no))
        .w_full()
        .gap(space::MD)
        .center_y()
        .px(space::MD)
        .py(space::SM)
        .child(
            text(title)
                .key(format!("album-disc-{}-title", d.disc_no))
                .font(type_::TITLE)
                .color(p.fg)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        )
        .child(
            text(fmt_total_dur(d.duration_secs))
                .key(format!("album-disc-{}-dur", d.disc_no))
                .font(type_::CAPTION)
                .color(p.text_secondary),
        )
}

/// An album's detail page (the old app's AlbumPage): a back link, then the shared
/// hero, square cover, "ALBUM" eyebrow, title, artist, "{n} tracks · {duration}"
/// and a Play-All pill, over the album's numbered track list. The album is
/// `State::open_album`, re-derived from the current library; if it has vanished we
/// just offer the way back.
pub fn album_detail(s: &State, p: &Palette, scale: f32) -> El {
    // Reuse the memoized album grouping (keyed by the library fingerprint) instead
    // of re-grouping the whole library every frame to locate one album.
    let albums = s.grouped_albums();
    let Some(album) = s
        .open_album
        .as_ref()
        .and_then(|r| resolve_album(&albums, r).cloned())
    else {
        return col()
            .key("album-detail")
            .grow(1.0)
            .min_h(0.0)
            .padding(space::PANE);
    };

    let total: f64 = album
        .ids
        .iter()
        .filter_map(|id| s.track(*id))
        .map(|t| t.duration_secs)
        .sum();
    let mut meta = format!(
        "{} · {}",
        crate::views::library::tracks_label(album.ids.len()),
        fmt_total_dur(total)
    );
    // A reissue note, but only when it genuinely supplements the year already
    // shown in the subtitle above, e.g. a 2015 remaster of a 1973 album.
    // Compare YEARS (not full packed dates): mixed-precision tags are common
    // (a MusicBrainz original date with only a year, alongside a locally
    // tagged release date with a full day) and must not trigger a redundant
    // "reissued 1973" next to a subtitle that already reads "1973". The note
    // only fires when the subtitle is currently showing the original year,
    // in Release mode, or when the original is unknown and the subtitle falls
    // back to the release year, the release year is already on screen.
    let orig_year = (album.original_date != 0).then_some((album.original_date >> 9) as u16);
    let rel_year = (album.release_date != 0).then_some((album.release_date >> 9) as u16);
    if let (Some(oy), Some(ry)) = (orig_year, rel_year)
        && oy != ry
        && album_year(&album, s.album_year) == Some(oy)
    {
        meta = format!("{meta} · {}", t!("detail.reissued", year => ry));
    }
    // The album's overall dynamic range (mean of its analyzed tracks' DR), once at least
    // one track is analyzed, the same figure the grid card's DR badge carries.
    if let Some(dr) = album.dr {
        meta = format!("{meta} · DR{}", dr.round() as i32);
    }

    // A user-uploaded custom cover (the old app's "Edit Artwork") wins over the
    // cover-track art, the same precedence the album grid card uses.
    let art = match s.covers.album_cover_path(&album.name) {
        Some(path) => HeroArt::Image {
            path,
            circular: false,
        },
        None => HeroArt::Cover {
            id: album.cover_id,
            circular: false,
        },
    };
    let members: Vec<&TrackView> = album.ids.iter().filter_map(|id| s.track(*id)).collect();
    let discs = soul_player_host::album_tracklist(&members);
    // Play exactly the list the page shows: the rendered rows in (disc, track)
    // order, so a row's play-start index can never drift from `Album::ids` (e.g. a
    // grouping that lags the library by an id the library no longer resolves).
    let play_ids: Vec<u32> = discs
        .iter()
        .flat_map(|d| d.rows.iter().map(|r| r.0))
        .collect();
    let hero = detail_hero(
        s,
        p,
        scale,
        "album",
        art,
        t!("album.eyebrow").to_uppercase(),
        album.name.clone(),
        album.remote_source.clone(),
        album_subtitle(&album, s.album_year),
        meta,
        Some(&crate::views::icons::CLOCK),
        detail_actions("album-play-all", play_ids.clone(), p),
        Some(ArtworkTarget::Album(album.name.clone())),
    );

    let show_disc_headers = album.disc_count > 1 && discs.len() > 1;
    let mut list = col().key("album-tracks").role("list").gap(space::XXS);
    // `i` indexes `play_ids`: it advances only for rows actually added.
    let mut i = 0usize;
    for d in &discs {
        if show_disc_headers {
            list = list.child(disc_header(d, p));
        }
        for &(id, number) in &d.rows {
            if let Some(t) = s.track(id) {
                list = list.child(detail_track_row(
                    s.now_playing_id,
                    s.playing,
                    p,
                    &play_ids,
                    i,
                    Some(number),
                    t,
                    false, // album detail: artist is constant
                    false, // album detail: album is constant
                ));
                i += 1;
            }
        }
    }

    col().key("album-detail").grow(1.0).min_h(0.0).child(
        col()
            .key("album-detail-content")
            .padding(space::PANE)
            .child(hero)
            .child(detail_track_header(p, false, false))
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

    fn ktv(id: u32, album: &str, key: u32, disc: u16, track: u16) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some("Guest".into()),
            album: Some(album.into()),
            album_key: key,
            disc_no: disc,
            track_no: track,
            disc_count: 1,
            duration_secs: 60.0,
            ..Default::default()
        }
    }

    #[test]
    fn same_title_in_two_folders_stays_two_albums() {
        let lib = vec![
            ktv(1, "Greatest Hits", 10, 1, 1),
            ktv(2, "Greatest Hits", 20, 1, 1),
        ];
        let albums = group_albums(&lib);
        assert_eq!(albums.len(), 2, "grouped by identity, not by title");
        assert_eq!((albums[0].key, albums[1].key), (10, 20));
        assert_eq!(albums[0].name, albums[1].name);
    }

    #[test]
    fn cd1_cd2_is_one_album_with_two_discs_in_disc_track_order() {
        // Library order (e.g. Title axis) scrambles the album's tracks.
        let mut lib = vec![
            ktv(3, "Box", 7, 2, 1),
            ktv(2, "Box", 7, 1, 2),
            ktv(1, "Box", 7, 1, 1),
        ];
        for t in &mut lib {
            t.disc_count = 2;
        }
        let albums = group_albums(&lib);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].disc_count, 2);
        assert_eq!(albums[0].ids, vec![1, 2, 3], "play order is (disc, track)");
    }

    #[test]
    fn card_artist_is_the_album_artist_falling_back_to_the_track_artist() {
        let mut with = ktv(1, "Box", 7, 1, 1);
        with.album_artist = Some("HOYO-MiX".into());
        let without = ktv(2, "Solo", 8, 1, 1);
        let albums = group_albums(&[with, without]);
        assert_eq!(albums[0].artist, "HOYO-MiX");
        assert_eq!(
            albums[1].artist, "Guest",
            "no album artist: the track artist"
        );
    }

    #[test]
    fn resolve_album_prefers_key_and_falls_back_to_title_after_a_rescan() {
        let albums = group_albums(&[ktv(1, "Hits", 10, 1, 1), ktv(2, "Hits", 20, 1, 1)]);
        let r = crate::AlbumRef {
            key: 20,
            name: "Hits".into(),
        };
        assert_eq!(resolve_album(&albums, &r).map(|a| a.key), Some(20));
        // A rescan renumbered the album ids: the title still finds an album.
        let stale = crate::AlbumRef {
            key: 99,
            name: "Hits".into(),
        };
        assert_eq!(resolve_album(&albums, &stale).map(|a| a.key), Some(10));
        let gone = crate::AlbumRef {
            key: 10,
            name: "Nope".into(),
        };
        assert!(resolve_album(&albums, &gone).is_none());
    }

    /// Click the element with `key`: find its position, drive a full
    /// move/down/up pointer gesture there, then re-render.
    fn click(rt: &mut rune::Runtime<State>, key: &str) {
        let (x, y) = rt
            .find(key)
            .unwrap_or_else(|| panic!("element {key} not found"));
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    fn atv(id: u32, artist: &str, album: &str) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some(artist.into()),
            album: Some(album.into()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: None,
            album_key: crate::views::albums::test_album_key(Some(album)),
            ..Default::default()
        }
    }

    /// A realistic library of `n` tracks: ~12 tracks per album, ~8 albums per
    /// artist, so grouping produces a believable album/artist fan-out.
    fn synth_library(n: u32) -> Vec<TrackView> {
        (0..n)
            .map(|i| {
                let album_idx = i / 12;
                let artist_idx = album_idx / 8;
                atv(
                    i,
                    &format!("Artist {artist_idx}"),
                    &format!("Album {album_idx}"),
                )
            })
            .collect()
    }

    /// Benchmark: how long per-render grouping + filtering takes as the library
    /// scales toward millions. `#[ignore]` so it never runs in the normal suite;
    /// run with `cargo test -p soul-player-rune bench_grouping -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_grouping_at_scale() {
        use std::time::Instant;
        for &n in &[10_000u32, 100_000, 1_000_000] {
            let lib = synth_library(n);
            let t = Instant::now();
            let albums = group_albums(&lib);
            let d_group = t.elapsed();
            let q = "album 5".to_string();
            let t = Instant::now();
            let shown = filter_albums(albums.clone(), &q);
            let d_filter = t.elapsed();
            eprintln!(
                "n={n:>9}  group_albums={d_group:>10.2?} ({} albums)  filter={d_filter:>10.2?} ({} shown)",
                albums.len(),
                shown.len()
            );
        }
    }

    /// The Albums grid is virtualized: a 100k-track library (~8k albums) mounts
    /// only a bounded WINDOW of card rows, not all ~1400 rows, so the page stays
    /// smooth at scale. Also exercises the memoized grouping (one `group_albums`,
    /// not one per frame).
    #[test]
    fn albums_grid_virtualizes_a_huge_library() {
        use rune::{Env, Runtime};
        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            albums_page(s, &p, cx, cx.env().scale)
        }
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = synth_library(100_000);
        s.route = crate::Route::Albums;
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 900.0), env);
        rt.frame(0.0);
        let rows = rt
            .elements()
            .into_iter()
            .filter(|e| {
                e.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("albums-grid-row-"))
            })
            .count();
        assert!(
            (1..60).contains(&rows),
            "the grid windows its rows at 100k tracks, got {rows}"
        );
    }

    /// An album's `remote_source` is the unanimous host across every one of
    /// its tracks; a mix of remote and local has no consensus and must show
    /// no badge.
    #[test]
    fn group_albums_resolves_remote_source_consensus() {
        let mut remote1 = atv(1, "Bach", "Cello Suites");
        remote1.remote_source = Some("h".to_string());
        let mut remote2 = atv(2, "Bach", "Cello Suites");
        remote2.remote_source = Some("h".to_string());
        let all_remote = group_albums(&[remote1, remote2]);
        assert_eq!(
            all_remote[0].remote_source,
            Some("h".to_string()),
            "every track shares the same host"
        );

        let mut remote = atv(3, "Bach", "Goldberg");
        remote.remote_source = Some("h".to_string());
        let local = atv(4, "Bach", "Goldberg");
        let mixed = group_albums(&[remote, local]);
        assert_eq!(
            mixed[0].remote_source, None,
            "a local track in the mix breaks consensus"
        );

        let all_local = group_albums(&[atv(5, "Bach", "Partitas"), atv(6, "Bach", "Partitas")]);
        assert_eq!(all_local[0].remote_source, None, "all local -> no badge");
    }

    #[test]
    fn total_duration_rounds_to_whole_minutes_and_formats_hours() {
        let _locale = crate::config::use_test_locale("en");
        assert_eq!(fmt_total_dur(45.0 * 60.0), "45 min");
        assert_eq!(fmt_total_dur(83.0 * 60.0), "1 h 23 min");
        assert_eq!(fmt_total_dur(0.0), "0 min");
        assert_eq!(fmt_total_dur(29.0), "0 min", "under 30 s rounds down");
        assert_eq!(fmt_total_dur(31.0), "1 min", "over 30 s rounds up");
    }

    #[test]
    fn album_year_prefers_selected_source_with_fallback() {
        use crate::config::AlbumYearSource::*;
        let a = Album {
            key: 1,
            name: "x".into(),
            artist: "y".into(),
            disc_count: 1,
            cover_id: 0,
            ids: vec![],
            original_date: 1973u32 << 9,
            release_date: 2015u32 << 9,
            remote_source: None,
            dr: None,
            genres: Vec::new(),
        };
        assert_eq!(album_year(&a, Original), Some(1973));
        assert_eq!(album_year(&a, Release), Some(2015));
        // fallback: release known, original unknown, Original selected → release
        let b = Album {
            original_date: 0,
            release_date: 2015u32 << 9,
            ..a.clone()
        };
        assert_eq!(album_year(&b, Original), Some(2015));
        // both unknown → None
        let c = Album {
            original_date: 0,
            release_date: 0,
            ..a.clone()
        };
        assert_eq!(album_year(&c, Original), None);
    }

    #[test]
    fn album_dr_means_analyzed_tracks_and_is_none_when_unanalyzed() {
        // The mean of the analyzed members' DR (6, 12, 18 → 12.0).
        assert_eq!(album_dr([6.0, 12.0, 18.0]), Some(12.0));
        assert_eq!(album_dr([8.5]), Some(8.5));
        // No analyzed member tracks yet → no album DR.
        assert_eq!(album_dr(std::iter::empty::<f32>()), None);
    }

    #[test]
    fn group_albums_means_the_member_tracks_dr() {
        let dr_track = |id: u32, album: &str, dr: Option<f32>| {
            let mut t = atv(id, "A", album);
            t.loudness = dr.map(|d| soul_player_host::TrackLoudness {
                integrated_lufs: -14.0,
                lra: 5.0,
                true_peak_dbtp: -1.0,
                plr: 8.0,
                dr: d,
            });
            t
        };
        let albums = group_albums(&[
            dr_track(1, "X", Some(6.0)),
            dr_track(2, "X", Some(12.0)), // album X: mean(6, 12) = 9.0
            dr_track(3, "Y", None),       // album Y: nothing analyzed yet
        ]);
        assert_eq!(
            albums[0].dr,
            Some(9.0),
            "album DR is the mean of its analyzed tracks"
        );
        assert_eq!(albums[1].dr, None, "an all-un-analyzed album has no DR");
    }

    #[test]
    fn albums_of_artist_folds_only_that_artists_albums_first_seen() {
        let _locale = crate::config::use_test_locale("en");
        let lib = vec![
            atv(1, "Bach", "Cello Suites"),
            atv(2, "Satie", "Gymnopedies"),
            atv(3, "Bach", "Cello Suites"),
            atv(4, "Bach", "Goldberg"),
        ];
        let albums = albums_of_artist(&lib, "Bach", crate::config::AlbumYearSource::Original);
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

    /// The Discography must re-sort by whichever date is currently selected:
    /// A is earlier by original date, B is earlier by release date, so the two
    /// toggle positions must disagree on the order.
    #[test]
    fn discography_sorts_by_the_selected_date() {
        use crate::config::AlbumYearSource::*;
        let _locale = crate::config::use_test_locale("en");
        let mut a = atv(1, "Artist", "A");
        a.original_date = 1990u32 << 9;
        a.release_date = 2010u32 << 9;
        let mut b = atv(2, "Artist", "B");
        b.original_date = 1980u32 << 9;
        b.release_date = 2020u32 << 9;
        let lib = vec![a, b];

        let by_original = albums_of_artist(&lib, "Artist", Original);
        assert_eq!(
            by_original
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["B", "A"],
            "1980 < 1990"
        );

        let by_release = albums_of_artist(&lib, "Artist", Release);
        assert_eq!(
            by_release
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"],
            "2010 < 2020"
        );
    }

    /// End-to-end through the real Albums grid: a seeded 1973 album's card
    /// shows "Artist · 1973"; an album with no known year falls back to the
    /// artist alone, never a stray "· 0".
    #[test]
    fn album_card_renders_the_year_or_falls_back_to_the_artist_alone() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            albums_page(s, &p, cx, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut dated = atv(1, "Pavement", "Crooked Rain");
        dated.original_date = 1973u32 << 9;
        let undated = atv(2, "NoYear", "Mystery");

        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![dated, undated];
        s.route = crate::Route::Albums;
        // Pin the source rather than trust the ambient persisted config.
        s.album_year = crate::config::AlbumYearSource::Original;

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(
            texts.iter().any(|t| t == "Pavement · 1973"),
            "a known year renders as \"Artist · year\"; got {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "NoYear"),
            "an unknown year falls back to the artist alone; got {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("· 0")),
            "never render a bare zero year; got {texts:?}"
        );
    }

    /// Through the real Albums grid: an album whose tracks are analyzed shows a DR badge on
    /// its cover carrying the mean DR; an un-analyzed album shows none.
    #[test]
    fn album_card_shows_the_mean_dr_badge_when_analyzed() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            albums_page(s, &p, cx, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let dr_track = |id: u32, album: &str, dr: Option<f32>| {
            let mut t = atv(id, "A", album);
            t.loudness = dr.map(|d| soul_player_host::TrackLoudness {
                integrated_lufs: -14.0,
                lra: 5.0,
                true_peak_dbtp: -1.0,
                plr: 8.0,
                dr: d,
            });
            t
        };

        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        // Album "X": mean(6, 12) = 9 → "DR9". Album "Y": un-analyzed → no badge.
        s.library = vec![
            dr_track(1, "X", Some(6.0)),
            dr_track(2, "X", Some(12.0)),
            dr_track(3, "Y", None),
        ];
        s.route = crate::Route::Albums;
        s.show_loudness_columns = true; // DR badges follow this toggle

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        let dr_badges: Vec<&String> = texts.iter().filter(|t| t.starts_with("DR")).collect();
        assert_eq!(
            dr_badges,
            vec!["DR9"],
            "exactly the analyzed album shows its mean-DR badge; got {texts:?}"
        );

        // The DR badge follows the loudness-columns toggle, same as the Tracks
        // page: turning it off hides the badge on the album grid cards.
        rt.update(|st: &mut State, _tx: &mut rune::Tx| st.show_loudness_columns = false);
        rt.frame(0.0);
        // Skip `exiting` elements: the removed badge lingers mid exit-animation
        // with its stale "DR9" text until it settles.
        let after: Vec<String> = rt
            .elements()
            .into_iter()
            .filter(|e| !e.exiting)
            .filter_map(|e| e.text)
            .collect();
        assert!(
            !after.iter().any(|t| t.starts_with("DR")),
            "loudness off -> no DR badge on the album cards; got {after:?}"
        );
    }

    /// End-to-end through the real Albums grid: a remote album's card shows
    /// the same cloud badge + "streams from" tooltip the track rows and
    /// now-playing line already show, keyed off the card rather than a track
    /// id; a local album's card shows none.
    #[test]
    fn album_card_shows_the_remote_badge_for_a_remote_album() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            albums_page(s, &p, cx, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut remote_track = atv(1, "Remote Artist", "Remote Album");
        remote_track.remote_source = Some("listen.home.lan".into());
        let local_track = atv(2, "Local Artist", "Local Album");

        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![remote_track, local_track];
        s.route = crate::Route::Albums;

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let badge_visible = |rt: &Runtime<State>, key: &str| {
            rt.elements()
                .iter()
                .any(|e| e.key.as_deref() == Some(key) && e.opacity > 0.01)
        };
        assert!(
            badge_visible(&rt, "album-0-remote"),
            "the remote album's card should show the cloud badge"
        );
        assert!(
            !badge_visible(&rt, "album-1-remote"),
            "the local album's card should show no badge"
        );
        let help = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("album-0-remote"))
            .and_then(|e| e.help);
        assert_eq!(
            help.as_deref(),
            Some("Streams from listen.home.lan"),
            "the card badge's tooltip should name the source host"
        );
    }

    /// A remote album's detail-page header shows the same cloud badge +
    /// tooltip next to the title; a local album's header shows none.
    #[test]
    fn album_detail_header_shows_the_remote_badge_for_a_remote_album() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            album_detail(s, &p, scale)
        }

        fn render(s: State) -> Runtime<State> {
            let env = Env {
                system_dark: true,
                scale: 2.0,
            };
            let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
            rt.frame(0.0);
            rt
        }

        let _locale = crate::config::use_test_locale("en");

        let mut remote_track = atv(1, "Artist", "Remote Album");
        remote_track.remote_source = Some("listen.home.lan".into());
        let mut remote_state = State::new(soul_player_host::PlayerHandle::offline());
        remote_state.library = vec![remote_track];
        remote_state.route = crate::Route::Album;
        remote_state.open_album = Some(crate::AlbumRef::by_name("Remote Album"));
        let remote_rt = render(remote_state);
        let badge = remote_rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("album-remote"));
        assert!(
            badge.is_some(),
            "a remote album's header should show the cloud badge"
        );
        assert_eq!(
            badge.and_then(|e| e.help).as_deref(),
            Some("Streams from listen.home.lan"),
            "the header badge's tooltip should name the source host"
        );

        let local_track = atv(2, "Artist", "Local Album");
        let mut local_state = State::new(soul_player_host::PlayerHandle::offline());
        local_state.library = vec![local_track];
        local_state.route = crate::Route::Album;
        local_state.open_album = Some(crate::AlbumRef::by_name("Local Album"));
        let local_rt = render(local_state);
        assert!(
            !local_rt
                .elements()
                .iter()
                .any(|e| e.key.as_deref() == Some("album-remote")),
            "a local album's header should show no badge"
        );
    }

    /// The album detail header's meta line carries the album DR (mean of its analyzed
    /// tracks) once at least one track is analyzed.
    #[test]
    fn album_detail_header_shows_the_album_dr_when_analyzed() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            album_detail(s, &p, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let dr_track = |id: u32, dr: f32| {
            let mut t = atv(id, "Artist", "Analyzed Album");
            t.loudness = Some(soul_player_host::TrackLoudness {
                integrated_lufs: -14.0,
                lra: 5.0,
                true_peak_dbtp: -1.0,
                plr: 8.0,
                dr,
            });
            t
        };

        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![dr_track(1, 6.0), dr_track(2, 12.0)]; // mean = 9 → "DR9"
        s.route = crate::Route::Album;
        s.open_album = Some(crate::AlbumRef::by_name("Analyzed Album"));

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);

        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(
            texts.iter().any(|t| t.contains("DR9")),
            "the detail header meta includes the album DR; got {texts:?}"
        );
    }

    /// The album hero puts a clock icon before its "{n} tracks, {dur}" meta
    /// (the original's hero), rendered as an `album-meta-line` row. Guards the
    /// `meta_icon` threading through the shared `detail_hero`.
    #[test]
    fn album_detail_hero_has_the_clock_meta_line() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            album_detail(s, &p, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![atv(1, "Artist", "An Album"), atv(2, "Artist", "An Album")];
        s.route = crate::Route::Album;
        s.open_album = Some(crate::AlbumRef::by_name("An Album"));
        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("album-meta-line").is_some(),
            "the album hero meta renders as a clock-icon row"
        );
    }

    /// The album detail's "reissued" note must compare YEARS, not full packed
    /// dates, and must only fire when it adds information beyond the subtitle
    /// already shown. Renders the real detail page through a local test-only
    /// view fn (same pattern as the albums-grid render test above).
    #[test]
    fn reissue_note_only_fires_when_it_adds_a_year_beyond_the_subtitle() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            album_detail(s, &p, scale)
        }

        fn detail_texts(s: State) -> Vec<String> {
            let env = Env {
                system_dark: true,
                scale: 2.0,
            };
            let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
            rt.frame(0.0);
            rt.elements().into_iter().filter_map(|e| e.text).collect()
        }

        fn seeded(
            album_name: &str,
            original_date: u32,
            release_date: u32,
            src: crate::config::AlbumYearSource,
        ) -> State {
            let mut t = atv(1, "Can", album_name);
            t.original_date = original_date;
            t.release_date = release_date;
            let mut s = State::new(soul_player_host::PlayerHandle::offline());
            s.library = vec![t];
            s.route = crate::Route::Album;
            s.open_album = Some(crate::AlbumRef::by_name(album_name));
            s.album_year = src;
            s
        }

        let _locale = crate::config::use_test_locale("en");
        use crate::config::AlbumYearSource::{Original, Release};

        // The bug case: same year, different precision (a year-only
        // MusicBrainz original alongside a fully-dated local release). Must
        // NOT show a redundant "reissued 1973" next to a "1973" subtitle.
        let texts = detail_texts(seeded(
            "Future Days",
            1973u32 << 9,
            (1973u32 << 9) | (6 << 5) | 15,
            Original,
        ));
        assert!(
            !texts.iter().any(|t| t.contains("reissued")),
            "same-year mixed precision must not show a redundant reissue note; got {texts:?}"
        );

        // A genuine reissue: different years, Original selected (subtitle
        // shows 1973) → the release year is new information.
        let texts = detail_texts(seeded("Tago Mago", 1973u32 << 9, 2015u32 << 9, Original));
        assert!(
            texts.iter().any(|t| t.contains("reissued 2015")),
            "a genuine reissue year must be noted; got {texts:?}"
        );

        // Same dates, but Release selected: the subtitle already shows 2015,
        // so a "reissued 2015" note would be redundant.
        let texts = detail_texts(seeded("Tago Mago", 1973u32 << 9, 2015u32 << 9, Release));
        assert!(
            !texts.iter().any(|t| t.contains("reissued")),
            "release-year subtitle already shows the year; the note would be redundant; got {texts:?}"
        );
    }

    /// `filter_albums` keeps an album when the query substring-matches its
    /// name OR its artist (case-insensitively), and keeps everything for an
    /// empty query.
    #[test]
    fn filter_albums_matches_name_or_artist_case_insensitively() {
        let albums = group_albums(&[
            atv(1, "Pavement", "Crooked Rain"),
            atv(2, "Can", "Tago Mago"),
            atv(3, "Beck", "Mellow Gold"),
        ]);
        let by_name = filter_albums(albums.clone(), "mago");
        assert_eq!(
            by_name.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["Tago Mago"],
            "a name substring match (lowercased query) keeps only that album"
        );
        let by_artist = filter_albums(albums.clone(), "beck");
        assert_eq!(
            by_artist
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Mellow Gold"],
            "an artist substring match keeps that album even when the name doesn't match"
        );
        let empty_query = filter_albums(albums.clone(), "");
        assert_eq!(empty_query.len(), 3, "an empty query keeps every album");
        let no_match = filter_albums(albums, "zzz");
        assert!(no_match.is_empty(), "no match keeps nothing");
    }

    /// End-to-end through the real Albums grid: the search box renders above
    /// the grid, and typing a query that matches only one album's name or
    /// artist narrows the grid down to it; clearing it shows every album again.
    #[test]
    fn albums_search_narrows_the_grid() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            albums_page(s, &p, cx, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![
            atv(1, "Pavement", "Crooked Rain"),
            atv(2, "Can", "Tago Mago"),
        ];
        s.route = crate::Route::Albums;

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("albums-search").is_some(),
            "the search box renders above the grid"
        );
        // Match by rendered text, not slot key: a filtered grid re-indexes its
        // remaining cards from 0, so "album-0" exists regardless of which
        // album survived, only the text proves it's the right one. Skip
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
            texts(&rt).iter().any(|t| t == "Crooked Rain")
                && texts(&rt).iter().any(|t| t == "Tago Mago"),
            "both albums show with no query"
        );

        rt.update(|st: &mut State, _tx: &mut Tx| st.album_search = rune::TextEdit::new("tago"));
        rt.frame(0.0);
        assert!(
            texts(&rt).iter().any(|t| t == "Tago Mago"),
            "the name match stays"
        );
        assert!(
            !texts(&rt).iter().any(|t| t == "Crooked Rain"),
            "the non-match is filtered out of the grid"
        );

        // The query also matches by artist, not just album name.
        rt.update(|st: &mut State, _tx: &mut Tx| st.album_search = rune::TextEdit::new("pavement"));
        rt.frame(0.0);
        assert!(
            texts(&rt).iter().any(|t| t == "Crooked Rain"),
            "an artist-name match keeps that album"
        );
        assert!(!texts(&rt).iter().any(|t| t == "Tago Mago"));

        rt.update(|st: &mut State, _tx: &mut Tx| st.album_search = rune::TextEdit::new(""));
        rt.frame(0.0);
        assert!(
            texts(&rt).iter().any(|t| t == "Crooked Rain")
                && texts(&rt).iter().any(|t| t == "Tago Mago"),
            "clearing the query shows every album again"
        );
    }

    /// The Filters button toggles the genre-chip row, and selecting a genre
    /// narrows the grid to albums that have a track in that genre.
    #[test]
    fn albums_genre_filter_narrows_the_grid() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            albums_page(s, &p, cx, cx.env().scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        let mut rock = atv(1, "Pavement", "Crooked Rain");
        rock.genre = Some("Rock".into());
        let mut jazz = atv(2, "Can", "Tago Mago");
        jazz.genre = Some("Jazz".into());
        s.library = vec![rock, jazz];
        s.route = crate::Route::Albums;

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);
        assert!(
            rt.find("albums-filters-toggle").is_some(),
            "the Filters button renders"
        );
        assert!(
            rt.find("albums-genre-chip-0").is_none(),
            "chips stay hidden until Filters is opened"
        );

        rt.update(|st: &mut State, _tx: &mut Tx| st.albums_filters_open = true);
        rt.frame(0.0);
        assert!(
            rt.find("albums-genre-chip-0").is_some(),
            "opening Filters reveals the genre chips"
        );

        let texts = |rt: &Runtime<State>| -> Vec<String> {
            rt.elements()
                .into_iter()
                .filter(|e| !e.exiting)
                .filter_map(|e| e.text)
                .collect()
        };
        rt.update(|st: &mut State, _tx: &mut Tx| {
            st.album_genre_filter.insert("Jazz".into());
        });
        rt.frame(0.0);
        assert!(
            texts(&rt).iter().any(|t| t == "Tago Mago"),
            "the selected-genre album stays"
        );
        assert!(
            !texts(&rt).iter().any(|t| t == "Crooked Rain"),
            "an album with no track in the selected genre is filtered out"
        );

        rt.update(|st: &mut State, _tx: &mut Tx| st.album_genre_filter.clear());
        rt.frame(0.0);
        assert!(
            texts(&rt).iter().any(|t| t == "Crooked Rain")
                && texts(&rt).iter().any(|t| t == "Tago Mago"),
            "clearing the genre filter shows every album again"
        );
    }

    /// The Albums filter state is page-scoped: navigating away clears it.
    #[test]
    fn albums_filter_state_resets_on_nav() {
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.albums_filters_open = true;
        s.album_genre_filter.insert("Jazz".into());
        crate::go_back(&mut s);
        assert!(
            !s.albums_filters_open && s.album_genre_filter.is_empty(),
            "go_back clears the Albums filter state"
        );
        s.albums_filters_open = true;
        s.album_genre_filter.insert("Rock".into());
        crate::navigate_to(&mut s, crate::Route::Albums, None, None, None, None);
        assert!(
            !s.albums_filters_open && s.album_genre_filter.is_empty(),
            "navigate_to clears the Albums filter state"
        );
    }

    /// End-to-end through the real album-detail page: the column-header row
    /// renders above the track list, with TITLE/FORMAT/DURATION labels.
    #[test]
    fn album_detail_header_shows_the_column_labels() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            album_detail(s, &p, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut t = atv(1, "Broke For Free", "Directionless EP");
        t.format = Some("MP3".into());
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![t];
        s.route = crate::Route::Album;
        s.open_album = Some(crate::AlbumRef::by_name("Directionless EP"));

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("album-tracks-header").is_some(),
            "the header row renders above the track list"
        );
        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(texts.iter().any(|t| t == "TITLE"), "TITLE label renders");
        assert!(texts.iter().any(|t| t == "FORMAT"), "FORMAT label renders");
        assert!(
            texts.iter().any(|t| t == "DURATION"),
            "DURATION label renders"
        );
    }

    /// A two-disc album (disc 1: ids 1, 2; disc 2: id 3) on its open detail page,
    /// over a recording host so the play commands can be read back.
    fn two_disc_album_page() -> State {
        let mut lib = vec![
            ktv(1, "Box", 7, 1, 1),
            ktv(2, "Box", 7, 1, 2),
            ktv(3, "Box", 7, 2, 1),
        ];
        for t in &mut lib {
            t.disc_count = 2;
        }
        lib[2].duration_secs = 180.0;
        let mut s = State::new(soul_player_host::PlayerHandle::recording());
        s.library = lib;
        s.route = crate::Route::Album;
        s.open_album = Some(crate::AlbumRef {
            key: 7,
            name: "Box".into(),
        });
        s
    }

    fn render_album_page(s: State) -> rune::Runtime<State> {
        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            album_detail(s, &p, scale)
        }
        let env = rune::Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = rune::Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);
        rt
    }

    /// The `PlayContext` commands the recording host received.
    fn play_contexts(rt: &rune::Runtime<State>) -> Vec<(Vec<u32>, u32)> {
        rt.state
            .host
            .recorded_commands()
            .into_iter()
            .filter_map(|c| match c {
                Command::PlayContext { ids, start } => Some((ids, start)),
                _ => None,
            })
            .collect()
    }

    /// Clicking a disc-2 row plays that track, within the album's (disc, track)
    /// list; disc 2's header shows its own duration.
    #[test]
    fn clicking_a_disc_two_row_plays_that_track() {
        let _locale = crate::config::use_test_locale("en");
        let mut rt = render_album_page(two_disc_album_page());
        let text_of = |rt: &rune::Runtime<State>, k: &str| {
            rt.elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some(k))
                .and_then(|e| e.text)
        };
        assert_eq!(text_of(&rt, "album-disc-2-dur").as_deref(), Some("3 min"));
        assert_eq!(text_of(&rt, "album-disc-1-dur").as_deref(), Some("2 min"));
        click(&mut rt, "album-track-2");
        assert_eq!(play_contexts(&rt), [(vec![1, 2, 3], 2)]);
    }

    /// The play list is the rows the page renders, not `Album::ids`: an id the
    /// library no longer resolves (a grouping that lags the library) must not
    /// shift which track a row starts.
    #[test]
    fn a_row_plays_its_own_track_when_the_grouping_lags_the_library() {
        let _locale = crate::config::use_test_locale("en");
        let s = two_disc_album_page();
        let mut album = group_albums(&s.library).remove(0);
        album.ids = vec![1, 99, 2, 3];
        let fp = s.library_fingerprint();
        *s.albums_cache.borrow_mut() = Some((fp, vec![album].into()));
        let mut rt = render_album_page(s);
        click(&mut rt, "album-track-2");
        assert_eq!(
            play_contexts(&rt),
            [(vec![1, 2, 3], 2)],
            "the disc-2 row starts track 3"
        );
    }

    /// A disc header only shows when the page really has more than one disc
    /// section (a two-disc album with only disc 1 in the library reads flat).
    #[test]
    fn no_disc_headers_when_only_one_disc_is_present() {
        let _locale = crate::config::use_test_locale("en");
        let mut s = two_disc_album_page();
        s.library.truncate(2);
        let rt = render_album_page(s);
        assert!(rt.find("album-track-1").is_some());
        assert!(rt.find("album-disc-1").is_none(), "one section: no header");
    }

    /// Clicking the detail hero's secondary "add to queue" button enqueues
    /// every one of the album's track ids, in order (the whole-album
    /// counterpart to the per-track `+` button on a Tracks row).
    #[test]
    fn detail_enqueue_all_button_enqueues_every_track_in_order() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            album_detail(s, &p, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::recording());
        s.library = vec![
            atv(1, "Broke For Free", "Directionless EP"),
            atv(2, "Broke For Free", "Directionless EP"),
        ];
        s.route = crate::Route::Album;
        s.open_album = Some(crate::AlbumRef::by_name("Directionless EP"));

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env);
        rt.frame(0.0);

        click(&mut rt, "detail-enqueue-all");

        let enqueued: Vec<u32> = rt
            .state
            .host
            .recorded_commands()
            .into_iter()
            .filter_map(|c| match c {
                Command::Enqueue(id) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(
            enqueued,
            vec![1, 2],
            "every one of the album's track ids enqueues, in library order"
        );
    }
}
