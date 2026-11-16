//! The shared detail-page hero + Play-All pill — the original soul-player's
//! `DetailPageLayout` header. Album/artist/genre detail pages all build their
//! header from `detail_hero` (artwork left, eyebrow/title/subtitle/meta/actions
//! right, reflowing to stacked on a narrow pane) and their primary action from
//! `play_all_pill`, so the three pages share one grammar.
#![forbid(unsafe_code)]

use rune::{Color, Cursor, El, Icon, col, el, icon, row, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::player::cover;

/// Large cover on a detail hero (the old app's `w-44`).
pub(crate) const DETAIL_COVER: f32 = 176.0;
/// The genre hero's icon tile (no artwork → a glyph in a muted square).
const ICON_TILE: f32 = 120.0;

/// A hero's artwork: a square album cover, a circular artist avatar, or a muted
/// icon tile (genres carry no art).
pub(crate) enum HeroArt {
    /// A cover drawn from a member track id; `circular` clips it to a circle.
    Cover { id: u32, circular: bool },
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn detail_hero(
    s: &State,
    p: &Palette,
    scale: f32,
    prefix: &str,
    art: HeroArt,
    eyebrow: String,
    title: String,
    subtitle: Option<String>,
    meta: String,
    actions: El,
) -> El {
    let mut info = col()
        .key(format!("{prefix}-meta"))
        .gap(space::XS)
        .grow(1.0)
        .min_w(0.0)
        .center_y()
        .child(text(eyebrow).font(type_::SECTION).color(p.text_secondary))
        .child(text(title).font(type_::DISPLAY).color(p.fg).truncate());
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
