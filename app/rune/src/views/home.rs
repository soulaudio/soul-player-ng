//! Home: a landing page with a library overview and quick actions — a greeting,
//! a one-line summary (track / album / artist counts), a Play-all button, and a
//! preview grid of the library's albums (click to open one). Our focused take on
//! the original's Home tab.
#![forbid(unsafe_code)]

use rune::{Color, Cursor, Cx, El, ScrollSpec, col, el, icon, row, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::group_albums;
use crate::views::artists::group_artists;
use crate::views::icons;
use crate::views::player::cover;

/// Side of a Home album preview tile.
const TILE: f32 = 132.0;
/// How many album tiles the preview shows.
const PREVIEW: usize = 12;

/// One album preview tile: a cover + the album name, opening the album's detail.
fn album_tile(s: &State, p: &Palette, slot: usize, name: String, cover_id: u32, scale: f32) -> El {
    let open = name.clone();
    col()
        .key(format!("home-album-{slot}"))
        .role("button")
        .label(name.clone())
        .focusable(true)
        .w(TILE)
        .gap(space::XS)
        .cursor(Cursor::Pointer)
        .hover_group()
        .child(
            el().key("home-tile-cover")
                .size(TILE, TILE)
                .radius(radius::MD)
                .shadow(2.0, 8.0, Color::BLACK.alpha(0.15))
                .child(cover(s, Some(cover_id), TILE, radius::MD, p, scale)),
        )
        .child(
            text(name)
                .font(type_::ITEM)
                .color(p.fg)
                .truncate()
                .group_opacity(1.0, 0.8),
        )
        .on_press(move |st: &mut State| {
            st.route = crate::Route::Album;
            st.open_album = Some(open.clone());
            st.mobile_show_content = true;
        })
}

/// The Home page: a hero (greeting + library summary + Play all) over a preview
/// grid of the library's albums. Empty libraries get a gentle "add a source"
/// nudge instead of the grid.
pub fn home_page(s: &State, p: &Palette, _cx: &Cx<State>, scale: f32) -> El {
    let albums = group_albums(&s.library);
    let tracks = s.library.len();
    // Count artists the same way the Artists page groups them (an Unknown bucket
    // included), so Home's summary matches that page.
    let artists = group_artists(&s.library).len();
    let play_ids: Vec<u32> = s.library.iter().map(|t| t.id).collect();
    let empty = tracks == 0;

    let summary = t!(
        "home.summary",
        tracks => tracks,
        albums => albums.len(),
        artists => artists,
    )
    .to_string();

    let mut hero = col()
        .key("home-hero")
        .gap(space::MD)
        .child(
            text(t!("home.greeting").to_string())
                .font(type_::DISPLAY)
                .color(p.fg),
        )
        .child(text(summary).font(type_::CAPTION).color(p.text_secondary));
    if !empty {
        // Wrap in a row so the pill takes its content width (left-aligned) rather
        // than stretching the full pane like a col child would.
        hero = hero.child(
            row().key("home-actions").child(
                // The same primary pill as the album/artist pages — plays the
                // whole library from the top.
                el().key("home-play-all")
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
    }

    let mut content = col()
        .key("home-content")
        .padding(space::PANE)
        .gap(space::SECTION)
        .child(hero);

    if empty {
        content = content.child(
            col()
                .key("home-empty")
                .gap(space::XS)
                .child(
                    text(t!("library.noMusic").to_string())
                        .font(type_::ITEM)
                        .color(p.text_secondary),
                )
                .child(
                    text(t!("library.noMusicHint").to_string())
                        .font(type_::CAPTION)
                        .color(p.text_secondary.alpha(0.7)),
                ),
        );
    } else {
        let mut section = col().key("home-albums-section").gap(space::MD).child(
            text(t!("nav.albums").to_string())
                .font(type_::TITLE)
                .color(p.fg),
        );
        let mut grid = row()
            .key("home-albums")
            .role("grid")
            .flex_wrap()
            .gap(space::GUTTER);
        for (slot, a) in albums.iter().take(PREVIEW).enumerate() {
            grid = grid.child(album_tile(s, p, slot, a.name.clone(), a.cover_id, scale));
        }
        section = section.child(grid);
        content = content.child(section);
    }

    col().key("home").grow(1.0).min_h(0.0).child(
        content
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .scroll_edge_fade(24.0)
            .grow(1.0)
            .min_h(0.0),
    )
}
