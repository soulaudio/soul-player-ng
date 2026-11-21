//! The shared detail-page hero + Play-All pill — the original soul-player's
//! `DetailPageLayout` header. Album/artist/genre detail pages all build their
//! header from `detail_hero` (artwork left, eyebrow/title/subtitle/meta/actions
//! right, reflowing to stacked on a narrow pane) and their primary action from
//! `play_all_pill`, so the three pages share one grammar.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use rune::{Color, Cursor, El, Icon, col, el, icon, row, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::player::{cover, cover_from_path, remote_badge_for};

/// Large cover on a detail hero (the old app's `w-44`).
pub(crate) const DETAIL_COVER: f32 = 176.0;
/// The genre hero's icon tile (no artwork → a glyph in a muted square).
const ICON_TILE: f32 = 120.0;

/// A hero's artwork: a square album cover, a circular artist avatar, an
/// on-disk image file (a playlist's custom cover), or a muted icon tile
/// (genres carry no art).
pub(crate) enum HeroArt {
    /// A cover drawn from a member track id; `circular` clips it to a circle.
    Cover { id: u32, circular: bool },
    /// A cover drawn from an image file on disk (a playlist's custom cover),
    /// rendered via the same path-keyed cache the Playlists list page uses.
    Image(PathBuf),
    /// A centered glyph in a muted `radius::MD` tile.
    Icon(&'static Icon),
}

fn hero_artwork(s: &State, p: &Palette, scale: f32, prefix: &str, art: HeroArt) -> El {
    match art {
        HeroArt::Cover { id, circular } => {
            let corner = if circular {
                DETAIL_COVER * 0.5
            } else {
                radius::MD
            };
            el().key(format!("{prefix}-cover"))
                .radius(corner)
                .shadow(4.0, 16.0, Color::BLACK.alpha(0.22))
                .child(cover(s, Some(id), DETAIL_COVER, corner, p, scale))
        }
        HeroArt::Image(path) => el()
            .key(format!("{prefix}-hero-image"))
            .radius(radius::MD)
            .shadow(4.0, 16.0, Color::BLACK.alpha(0.22))
            .child(cover_from_path(
                s,
                &path,
                DETAIL_COVER,
                radius::MD,
                p,
                scale,
            )),
        HeroArt::Icon(glyph) => el()
            .key(format!("{prefix}-cover"))
            .size(ICON_TILE, ICON_TILE)
            .radius(radius::MD)
            .fill(p.muted)
            .center()
            .child(icon(glyph, (ICON_TILE * 0.5).round()).color(p.text_secondary)),
    }
}

/// The shared detail header: artwork left, a text block right (eyebrow, title,
/// optional subtitle, a meta line, then `actions`). Reflows to stacked when the
/// pane is too narrow (`flex_wrap`). `prefix` keys the row/cover/meta so each page
/// keeps stable keys (`album-header`, `artist-cover`, …).
///
/// `remote_source` is `Some(host)` when the viewed album/artist/genre's tracks
/// all stream from that same remote host (`views::player::group_remote_source`,
/// read straight off the already-grouped struct) — it puts the same cloud badge
/// and "streams from {host}" tooltip the grid cards and track rows show next
/// to the title. `None` (a local or mixed-source group) shows nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn detail_hero(
    s: &State,
    p: &Palette,
    scale: f32,
    prefix: &str,
    art: HeroArt,
    eyebrow: String,
    title: String,
    remote_source: Option<String>,
    subtitle: Option<String>,
    meta: String,
    actions: El,
) -> El {
    let title_el = text(title).font(type_::DISPLAY).color(p.fg).truncate();
    // A local/mixed-source group (the common case) keeps the plain title; only a
    // group whose tracks all stream from one remote host gains the badge, same
    // "local is unremarked" rule as the grid cards and track rows.
    let title_row: El = if let Some(host) = &remote_source {
        row()
            .key(format!("{prefix}-title"))
            .gap(space::XS)
            .center_y()
            .min_w(0.0)
            .child(title_el.grow(1.0))
            .child(remote_badge_for(&format!("{prefix}-remote"), host, p))
    } else {
        title_el
    };
    let mut info = col()
        .key(format!("{prefix}-meta"))
        .gap(space::XS)
        .grow(1.0)
        .min_w(0.0)
        .center_y()
        .child(text(eyebrow).font(type_::SECTION).color(p.text_secondary))
        .child(title_row);
    if let Some(sub) = subtitle {
        info = info.child(
            text(sub)
                .font(type_::CARD_TITLE)
                .color(p.text_secondary)
                .truncate(),
        );
    }
    info = info
        .child(text(meta).font(type_::CAPTION).color(p.text_secondary))
        .child(actions);

    row()
        .key(format!("{prefix}-header"))
        .flex_wrap()
        .gap(space::XL)
        .pb(space::SECTION)
        .child(hero_artwork(s, p, scale, prefix, art))
        .child(info)
}

/// The "Play All" pill (the old app's `rounded-full px-6 py-3` filled Play
/// button) — plays `ids` from the top. `key` keeps each page's stable key
/// (`album-play-all`, `artist-play-all`, `genre-play-all`).
pub(crate) fn play_all_pill(key: &str, ids: Vec<u32>, p: &Palette) -> El {
    el().key(key.to_string())
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
                ids: ids.clone(),
                start: 0,
            })
        })
        .child(
            row()
                .gap(space::SM)
                .center_y()
                .child(icon(&icons::PLAY, 20.0).color(p.bg))
                .child(text(t!("album.playAll").to_string()).color(p.bg)),
        )
}

/// The size of the circular [`enqueue_all_button`] (roughly the old app's
/// secondary icon button, matched to sit comfortably beside the Play All pill).
const ENQUEUE_ALL_BTN: f32 = 44.0;

/// The secondary "add to queue" icon button beside Play All (the old app's
/// muted circular button next to its Play button): enqueues every one of
/// `ids`, in order, onto the override queue (the same queue the `+` button on
/// a Tracks row sends a single track to), just for a whole
/// album/artist/genre/playlist at once. `key` keeps each call site's stable
/// key.
pub(crate) fn enqueue_all_button(key: &str, ids: Vec<u32>, p: &Palette) -> El {
    el().key(key.to_string())
        .role("button")
        .label(t!("library.addToQueue").to_string())
        .focusable(true)
        .size(ENQUEUE_ALL_BTN, ENQUEUE_ALL_BTN)
        .center()
        .radius(radius::FULL)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(icon(&icons::LIST_PLUS, 18.0).color(p.text_secondary))
        .on_press(move |st: &mut State| {
            for &id in &ids {
                st.host.send(Command::Enqueue(id));
            }
        })
}

/// Play All plus the secondary "add to queue" button beside it: the shared
/// hero actions row for the album/artist/genre detail pages (the playlist
/// page builds its own richer action row, since it also carries rename/
/// delete/cover controls, but includes the same [`enqueue_all_button`]).
/// `play_key` is the page's stable Play All key (`album-play-all`, …); the
/// enqueue button shares one key (`detail-enqueue-all`) across pages, since
/// only one detail page is ever on screen at a time.
pub(crate) fn detail_actions(play_key: &str, ids: Vec<u32>, p: &Palette) -> El {
    row()
        .key(format!("{play_key}-actions"))
        .gap(space::SM)
        .center_y()
        .child(play_all_pill(play_key, ids.clone(), p))
        .child(enqueue_all_button("detail-enqueue-all", ids, p))
}
