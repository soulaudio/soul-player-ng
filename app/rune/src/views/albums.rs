//! Albums grid + an album's detail page, mirroring the old app's
//! AlbumsPage/MediaCard and AlbumPage. Each card is a square cover + title +
//! artist; the hover play button plays/pauses the album, the card body opens its
//! detail page. `TrackView` carries no year, so (unlike the original's "artist •
//! year") the card shows the artist alone.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use rune::{Color, Cursor, Cx, El, ScrollSpec, col, el, icon, row, text};
use soul_player_host::{Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::library::fmt_dur;
use crate::views::player::cover;

/// The hover play-overlay button — the old app's `w-14 h-14 rounded-xl` (56 px).
const PLAY_BTN: f32 = 56.0;
/// Smallest album-card width before the responsive grid drops a column. Tuned so
/// the column count tracks the old app's `grid-cols-2..8` breakpoints and cards
/// land ~144 px at a desktop width (its `lg` tier).
const MIN_CARD: f32 = 130.0;

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

/// The command an album card's play action issues, mirroring the old app's
/// MediaCard `handlePlayPause`: if a track of this album is the one playing,
/// toggle pause/resume; otherwise start the album from the top. Pure, so the
/// view and its test share exactly one rule. "Active" = the now-playing track
/// belongs to this album.
fn album_play_command(now_playing: Option<u32>, ids: &[u32]) -> Command {
    if now_playing.is_some_and(|id| ids.contains(&id)) {
        Command::TogglePlay
    } else {
        Command::PlayContext {
            ids: ids.to_vec(),
            start: 0,
        }
    }
}

/// One album card: a square cover, the title, and the artist. Clicking plays the
/// album, or toggles pause/resume if it is already the playing context.
fn album_card(s: &State, p: &Palette, slot: usize, a: &Album, card_w: f32, scale: f32) -> El {
    let ids = a.ids.clone();
    let name = a.name.clone();
    // When this album is the active, playing context the overlay shows a pause
    // glyph and its press toggles — the old app's MediaCard behaviour.
    let show_pause = s.now_playing_id.is_some_and(|id| a.ids.contains(&id)) && s.playing;
    col()
        .key(format!("album-{slot}"))
        .w(card_w)
        // No wrapper background or inset — the old app's MediaCard is `pb-2` with a
        // `mb-2` cover; the cover + the hover play-overlay are the affordance, not a
        // background pill.
        .gap(space::SM)
        .pb(space::SM)
        .cursor(Cursor::Pointer)
        .hover_group()
        .child(
            // Cover + a play button that fades in on card hover (old app's
            // `opacity-0 group-hover:opacity-100` MediaCard overlay). ONLY the
            // button plays/pauses; clicking the cover elsewhere (or the title)
            // opens the album's detail page — the old app's MediaCard split of
            // button = play, body = navigate.
            el().key("cover-area")
                .size(card_w, card_w)
                // The old app's MediaCard cover carries a `shadow` (Rune has no
                // group-driven shadow/scale, so the hover `shadow-md`/`scale-105`
                // lift isn't reproduced — the base shadow is).
                .radius(radius::MD)
                .shadow(2.0, 8.0, Color::BLACK.alpha(0.15))
                .child(cover(s, Some(a.cover_id), card_w, radius::MD, p, scale))
                .child(
                    el().key("play-overlay")
                        .size(card_w, card_w)
                        .absolute(0.0, 0.0)
                        .center()
                        .group_opacity(0.0, 1.0)
                        .child(
                            // Old app's `w-14 h-14 rounded-xl bg-black/50` button,
                            // darkening to ~black/70 on direct hover (its
                            // `hover:bg-black/70`). Carries the play/pause press.
                            el().key("play-btn")
                                .size(PLAY_BTN, PLAY_BTN)
                                .radius(radius::LG)
                                .fill(Color::BLACK.alpha(0.5))
                                .hover(Color::BLACK.alpha(0.3))
                                .cursor(Cursor::Pointer)
                                .center()
                                .on_press(move |st: &mut State| {
                                    st.host.send(album_play_command(st.now_playing_id, &ids))
                                })
                                .child(
                                    icon(
                                        if show_pause {
                                            &icons::PAUSE
                                        } else {
                                            &icons::PLAY
                                        },
                                        32.0,
                                    )
                                    .color(Color::WHITE),
                                ),
                        ),
                ),
        )
        .child(
            col()
                .key("album-text")
                .gap(space::XXS)
                .min_w(0.0)
                .child(
                    // Title dims to 0.8 on card hover (old app's
                    // `group-hover:opacity-[var(--hover-text-opacity)]`).
                    text(a.name.clone())
                        .font(type_::CARD_TITLE)
                        .color(p.fg)
                        .truncate()
                        .group_opacity(1.0, 0.8),
                )
                .child(
                    text(a.artist.clone())
                        .font(type_::ITEM)
                        .color(p.text_secondary)
                        .truncate(),
                ),
        )
        // The card body opens the album's detail page (the play button above owns
        // play/pause). The old app's MediaCard navigates on body click.
        .on_press(move |st: &mut State| {
            st.route = crate::Route::Album;
            st.open_album = Some(name.clone());
        })
}

/// Columns and per-card width for a content pane `pane_w` points wide: fill the
/// pane, dropping a column below [`MIN_CARD`]. Fed the pane width (window minus the
/// sidebar), the column count tracks the old app's `grid-cols-2..8` viewport
/// breakpoints. Pure, so the grid and its test share exactly one rule.
fn grid_layout(pane_w: f32) -> (usize, f32) {
    let avail = (pane_w - 2.0 * space::PANE).max(MIN_CARD);
    let cols = (((avail + space::GUTTER) / (MIN_CARD + space::GUTTER)).floor() as usize).max(1);
    let card_w = ((avail - (cols as f32 - 1.0) * space::GUTTER) / cols as f32).floor();
    (cols, card_w)
}

/// The Albums page: a responsive grid of album cards, scrolled vertically.
pub fn albums_page(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let albums = group_albums(&s.library);
    // Responsive grid (the old app's `grid-cols-2 sm:3 md:4 lg:5 xl:6 2xl:8`): fill
    // the content pane, adding columns as it widens. Read the pane's laid-out width
    // (minus the grid's own padding); the first frame has no prior layout, so fall
    // back to a sensible desktop width.
    let pane = cx.layout_size("albums").map(|[w, _]| w).unwrap_or(820.0);
    let card_w = grid_layout(pane).1;

    let mut grid = row()
        .key("albums-grid")
        .flex_wrap()
        .gap(space::GUTTER)
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

/// Large cover on the album detail page (the old app's `w-44`).
const DETAIL_COVER: f32 = 176.0;

/// A back link to the Albums grid — a `←` glyph. Our focused shell keeps no page
/// history, so it simply returns to the grid.
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
        .on_press(|st: &mut State| st.route = crate::Route::Albums)
}

/// One row on the album detail page: the track number (or a ⏸/▶ state glyph when
/// it's the current track), title and duration. Clicking plays the album from
/// that track, or toggles pause/resume when it is already the one playing.
fn detail_track_row(
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
    r.child(
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
    )
    .child(
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

/// An album's detail page (the old app's AlbumPage): a back link, then a header
/// — large cover, an "ALBUM" eyebrow, title, artist, track count and a Play
/// button — over the album's track list. The album is `State::open_album`,
/// re-derived from the current library; if it has vanished we just offer the
/// way back.
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

    let play_ids = album.ids.clone();
    // Header stacks (cover over info) when the pane is too narrow for them side
    // by side (the old app's `flex-col sm:flex-row`).
    let header = row()
        .key("album-header")
        .flex_wrap()
        .gap(space::XL)
        .pb(space::SECTION)
        .child(
            // Large cover with a `shadow-lg`, as the old app's AlbumPage.
            el().key("album-cover")
                .radius(radius::MD)
                .shadow(4.0, 16.0, Color::BLACK.alpha(0.22))
                .child(cover(
                    s,
                    Some(album.cover_id),
                    DETAIL_COVER,
                    radius::MD,
                    p,
                    scale,
                )),
        )
        .child(
            col()
                .key("album-meta")
                .gap(space::XS)
                .grow(1.0)
                .min_w(0.0)
                .center_y()
                .child(
                    text(t!("album.eyebrow").to_uppercase())
                        .font(type_::SECTION)
                        .color(p.text_secondary),
                )
                .child(
                    text(album.name.clone())
                        .font(type_::DISPLAY)
                        .color(p.fg)
                        .truncate(),
                )
                .child(
                    text(album.artist.clone())
                        .font(type_::CARD_TITLE)
                        .color(p.text_secondary)
                        .truncate(),
                )
                .child(
                    text(t!("album.trackCount", count => album.ids.len()).to_string())
                        .font(type_::CAPTION)
                        .color(p.text_secondary),
                )
                .child(
                    // The old app's "Play All" pill (`rounded-full px-6 py-3`,
                    // filled Play icon) — plays the whole album from the top.
                    el().key("album-play-all")
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

    let mut list = col().key("album-tracks").gap(space::XXS);
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

    #[test]
    fn an_active_album_toggles_play_pause_otherwise_plays_from_the_start() {
        // The old app's MediaCard play button toggles pause/resume when its own
        // context is the active one, and starts the entity otherwise. We judge
        // "active" by the now-playing track belonging to this album.
        assert_eq!(
            album_play_command(Some(6), &[5, 6, 7]),
            Command::TogglePlay,
            "a track of this album is playing → toggle"
        );
        assert_eq!(
            album_play_command(Some(9), &[5, 6, 7]),
            Command::PlayContext {
                ids: vec![5, 6, 7],
                start: 0
            },
            "a different album is playing → play this one from the top"
        );
        assert_eq!(
            album_play_command(None, &[5, 6, 7]),
            Command::PlayContext {
                ids: vec![5, 6, 7],
                start: 0
            },
            "nothing playing → play from the top"
        );
    }

    #[test]
    fn grid_fills_the_pane_with_more_columns_as_it_widens() {
        // Fed the content-pane width (window minus the ~294 pt sidebar), the column
        // count tracks the old app's grid-cols-2..8 viewport breakpoints, and cards
        // land ~144 pt at a desktop width (its `lg` tier).
        assert_eq!(grid_layout(606.0).0, 4, "md-ish pane → 4 columns");
        let (cols, card) = grid_layout(820.0);
        assert_eq!(cols, 5, "lg pane → 5 columns");
        assert!((card - 144.0).abs() <= 2.0, "lg cards ~144 pt, got {card}");
        assert_eq!(grid_layout(1306.0).0, 8, "2xl pane → 8 columns");
        // Never fewer than one column; the card never shrinks below the minimum.
        assert_eq!(grid_layout(200.0).0, 1);
        assert!(grid_layout(820.0).1 >= MIN_CARD);
    }
}
