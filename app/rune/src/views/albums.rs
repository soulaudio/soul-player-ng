//! Albums grid + an album's detail page, mirroring the old app's
//! AlbumsPage/MediaCard and AlbumPage. Each card is a square cover + title +
//! an "Artist · year" subtitle (the old app's "artist • year"), the year read
//! from `State.album_year`'s selected date with a fallback to the other one;
//! the hover play button plays/pauses the album, the card body opens its
//! detail page.
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
use crate::views::player::{group_remote_source, remote_badge};

/// One grouped album: display name, its artist, a track id to draw the cover
/// from, every track id (in library order) to play on click, the album's
/// original/release dates (packed `(year<<9)|(month<<5)|day`, `0` = unknown;
/// see [`album_year`]), and whether every one of its tracks streams from the
/// same remote host (`None` for a local or mixed-source album — see
/// [`group_remote_source`]).
#[derive(Clone)]
pub(crate) struct Album {
    pub(crate) name: String,
    pub(crate) artist: String,
    pub(crate) cover_id: u32,
    pub(crate) ids: Vec<u32>,
    pub(crate) original_date: u32,
    pub(crate) release_date: u32,
    pub(crate) remote_source: Option<String>,
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
                original_date: t.original_date,
                release_date: t.release_date,
                remote_source: None, // resolved below, once every member track is known
            });
            albums.len() - 1
        });
        albums[i].ids.push(t.id);
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
    albums
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
/// `group_artists`' empty→"Unknown" bucketing), fold by album in first-seen
/// order, then re-sort chronologically by the selected date source — the
/// artist page's Discography. Unknown dates (`0`) sort last; the sort is
/// stable, so equal/unknown years keep their first-seen order.
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
                .clone()
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| t!("library.unknown").to_string());
            a == artist
        })
        .cloned()
        .collect();
    let mut albums = group_albums(&subset);
    // Chronological by the SELECTED date; unknown (0) sorts last. Stable
    // keeps the first-seen order among equal/unknown years.
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
    let name = a.name.clone();
    media_card(
        s,
        p,
        CardSpec {
            key: format!("album-{slot}"),
            label: a.name.clone(),
            title: a.name.clone(),
            subtitle: album_subtitle(a, s.album_year),
            cover_id: a.cover_id,
            circular: false,
            play_ids: a.ids.clone(),
            remote_source: a.remote_source.clone(),
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
        // Scroll on the growing wrapper, NOT the grid itself. A flex-wrap grid that is
        // the growing scroll element has flexbox stretch its wrapped lines to fill the
        // height, spreading the rows apart whenever the library doesn't fill the window
        // (worse the more you zoom — fewer rows fit). A natural-height grid inside a
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
    if let Some(host) = &t.remote_source {
        r = r.child(remote_badge(t.id, host, p));
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
    let mut meta = t!(
        "detail.tracksAndDuration",
        count => album.ids.len(),
        dur => fmt_total_dur(total)
    )
    .to_string();
    // A reissue note, but only when it genuinely supplements the year already
    // shown in the subtitle above — e.g. a 2015 remaster of a 1973 album.
    // Compare YEARS (not full packed dates): mixed-precision tags are common
    // (a MusicBrainz original date with only a year, alongside a locally
    // tagged release date with a full day) and must not trigger a redundant
    // "reissued 1973" next to a subtitle that already reads "1973". The note
    // only fires when the subtitle is currently showing the original year —
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
        album.remote_source.clone(),
        album_subtitle(&album, s.album_year),
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
            remote_source: None,
            original_date: 0,
            release_date: 0,
        }
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
        rust_i18n::set_locale("en");
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
            name: "x".into(),
            artist: "y".into(),
            cover_id: 0,
            ids: vec![],
            original_date: 1973u32 << 9,
            release_date: 2015u32 << 9,
            remote_source: None,
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
    fn albums_of_artist_folds_only_that_artists_albums_first_seen() {
        rust_i18n::set_locale("en");
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
        rust_i18n::set_locale("en");
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
    /// artist alone — never a stray "· 0".
    #[test]
    fn album_card_renders_the_year_or_falls_back_to_the_artist_alone() {
        use rune::{Env, Runtime};

        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            albums_page(s, &p, cx, scale)
        }

        rust_i18n::set_locale("en");
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
        remote_state.open_album = Some("Remote Album".into());
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
        local_state.open_album = Some("Local Album".into());
        let local_rt = render(local_state);
        assert!(
            !local_rt
                .elements()
                .iter()
                .any(|e| e.key.as_deref() == Some("album-remote")),
            "a local album's header should show no badge"
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
            s.open_album = Some(album_name.to_string());
            s.album_year = src;
            s
        }

        rust_i18n::set_locale("en");
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
}
