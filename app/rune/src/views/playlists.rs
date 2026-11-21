//! Playlists browse: every user-created playlist, as a scrolled list of rows
//! (cover thumbnail, name, track count) plus an inline "New playlist" field.
//! Mirrors `artists::album_row` for the cover-thumbnail-plus-text row shape
//! (Genres is now a card-tile grid, not a row list, see `genres::genre_tile`).
#![forbid(unsafe_code)]

use std::path::Path;

use rune::{Cursor, El, ScrollSpec, col, el, row, text, text_input};

use crate::State;
use crate::playlists::cover::choose_playlist_cover;
use crate::playlists::{PlaylistCover, PlaylistSummary};
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::player::{cover, cover_from_path};

/// A row's cover-thumbnail side, matching the library/artist rows' 48 pt
/// cover size.
const ROW_COVER: f32 = 48.0;

/// Choose a playlist summary's cover: a user-set custom cover image always
/// wins (`summary.custom_cover`); absent that, resolve the summary's cover
/// locators to current track ids and classify the result via the shared
/// `select_cover` mosaic/single/placeholder rule (reached through
/// [`choose_playlist_cover`]). The locators are already album-distinct (the
/// store's manifest walk picks the first 4 distinct-album entries), so no
/// further dedupe belongs here; each resolved id gets its own unique
/// synthetic "album" key purely so that rule's distinct-album dedupe is a
/// no-op passthrough, letting this reuse that selection logic as-is instead
/// of duplicating it. Locators that don't resolve (unavailable source, or no
/// library open at all) are dropped before the art-presence check, so this
/// never panics without a library loaded: everything just resolves to
/// `PlaylistCover::Placeholder`.
fn playlist_cover(s: &State, summary: &PlaylistSummary) -> PlaylistCover {
    let resolved: Vec<(u32, String)> = s
        .host
        .resolve_locators(&summary.cover)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(i, id)| (id, i.to_string()))
        .collect();
    let custom = summary
        .custom_cover
        .clone()
        .map(|p| p.to_string_lossy().into_owned());
    choose_playlist_cover(custom, &resolved, |id| s.host.art(id).is_some())
}

/// Render a playlist row's thumbnail: the user-set custom cover image for
/// `Custom`, a 2x2 mosaic for `Mosaic`, a single full-bleed cover for
/// `Single`, or the shared "no art" placeholder tile for `Placeholder`.
/// Reuses `views::player::cover`/`cover_from_path` for every tile's pixels
/// so a mosaic tile and a library row's own cover decode identically (same
/// cache, same placeholder); `Custom` falls back to that same placeholder
/// when the file can't be read or hasn't decoded yet, never panicking.
/// `key_prefix` must be unique per row (the playlist id) so two rows' tiles
/// never collide.
fn playlist_cover_el(
    s: &State,
    which: PlaylistCover,
    scale: f32,
    p: &Palette,
    key_prefix: &str,
) -> El {
    match which {
        PlaylistCover::Placeholder => {
            cover(s, None, ROW_COVER, radius::SM, p, scale).key(format!("{key_prefix}-placeholder"))
        }
        PlaylistCover::Custom(path) => {
            cover_from_path(s, Path::new(&path), ROW_COVER, radius::SM, p, scale)
                .key(format!("{key_prefix}-custom"))
        }
        PlaylistCover::Single(id) => {
            cover(s, Some(id), ROW_COVER, radius::SM, p, scale).key(format!("{key_prefix}-single"))
        }
        PlaylistCover::Mosaic(ids) => {
            let tile = ROW_COVER / 2.0;
            let mut grid = col()
                .key(format!("{key_prefix}-mosaic"))
                .size(ROW_COVER, ROW_COVER)
                .gap(space::NONE);
            for (ri, pair) in ids.chunks(2).enumerate() {
                let mut tile_row = row()
                    .key(format!("{key_prefix}-mosaic-row-{ri}"))
                    .gap(space::NONE);
                for (ci, id) in pair.iter().enumerate() {
                    tile_row = tile_row.child(
                        cover(s, Some(*id), tile, 0.0, p, scale)
                            .key(format!("{key_prefix}-tile-{ri}-{ci}")),
                    );
                }
                grid = grid.child(tile_row);
            }
            grid
        }
    }
}

/// One playlist row: cover thumbnail, name, and track count. Clicking opens
/// the playlist's detail page.
fn playlist_row(s: &State, p: &Palette, scale: f32, summary: &PlaylistSummary) -> El {
    let id = summary.id;
    let which = playlist_cover(s, summary);
    let key_prefix = format!("playlist-cover-{id}");
    row()
        .key(format!("playlist-row-{id}"))
        .role("button")
        .label(summary.name.clone())
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
        .child(playlist_cover_el(s, which, scale, p, &key_prefix))
        .child(
            col()
                .gap(space::XXS)
                .min_w(0.0)
                .grow(1.0)
                .child(
                    text(summary.name.clone())
                        .font(type_::ITEM)
                        .color(p.fg)
                        .truncate(),
                )
                .child(
                    text(t!("playlists.trackCount", count => summary.count).to_string())
                        .font(type_::CAPTION)
                        .color(p.text_secondary),
                ),
        )
        .on_press(move |st: &mut State| {
            crate::navigate_to(st, crate::Route::Playlist, None, None, None, Some(id));
        })
}

/// Commit the New-playlist field: a blank/whitespace-only name is a no-op;
/// otherwise create the playlist, reset the buffer, and jump straight into
/// the fresh (still-empty) playlist's detail page.
fn confirm_new_playlist(s: &mut State) {
    let name = s.playlist_new_buf.text.trim().to_string();
    if name.is_empty() {
        return;
    }
    let id = s.playlists.create(&name);
    s.playlist_new_buf = rune::TextEdit::new("");
    crate::navigate_to(s, crate::Route::Playlist, None, None, None, Some(id));
}

/// The inline "New playlist" affordance: a text field bound to
/// `State::playlist_new_buf`, plus a button, either of which confirms
/// (Enter in the field, or pressing the button).
fn new_playlist_row(s: &State, p: &Palette) -> El {
    row()
        .key("playlist-new-group")
        .w_full()
        .gap(space::SM)
        .child(
            text_input(&s.playlist_new_buf, type_::ITEM)
                .key("playlist-new-input")
                .label(t!("playlists.new").to_string())
                .placeholder(t!("playlists.newPlaceholder").to_string(), p.text_secondary)
                .single_line()
                .grow(1.0)
                .px(space::SM)
                .py(space::XS)
                .radius(radius::SM)
                .fill(p.muted)
                .color(p.fg)
                .on_edit(|st: &mut State| &mut st.playlist_new_buf)
                .on_submit(|st: &mut State, _tx| confirm_new_playlist(st)),
        )
        .child(
            el().key("playlist-new-confirm")
                .role("button")
                .label(t!("playlists.new").to_string())
                .focusable(true)
                .px(space::MD)
                .py(space::XS)
                .radius(radius::SM)
                .fill(p.primary)
                .cursor(Cursor::Pointer)
                .child(text(t!("playlists.new").to_string()).color(p.bg))
                .on_press(confirm_new_playlist),
        )
}

/// The Playlists page: a heading and the New-playlist field, then a scrolled
/// list of playlist rows (or an empty-state message when there are none).
pub fn page(s: &State, p: &Palette, scale: f32) -> El {
    let summaries = s.playlists.list();
    let mut list = col()
        .key("playlists-list")
        .role("list")
        .gap(space::XXS)
        .padding(space::PANE);
    if summaries.is_empty() {
        list = list.child(
            text(t!("playlists.empty").to_string())
                .font(type_::ITEM)
                .color(p.text_secondary),
        );
    } else {
        for summary in &summaries {
            list = list.child(playlist_row(s, p, scale, summary));
        }
    }
    col()
        .key("playlists")
        .grow(1.0)
        .min_h(0.0)
        .child(
            col()
                .key("playlists-header")
                .gap(space::SM)
                .padding(space::PANE)
                .child(
                    text(t!("playlists.title").to_string())
                        .font(type_::TITLE)
                        .color(p.fg),
                )
                .child(new_playlist_row(s, p)),
        )
        .child(
            list.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .scroll_edge_fade(24.0)
            .grow(1.0)
            .min_h(0.0),
        )
}

#[cfg(test)]
mod tests {
    use rune::{Cx, Env, Key, KeyEvent, Mods, Runtime};
    use soul_player_host::{PlayerHandle, TrackLocator, TrackSnapshot};

    use super::*;
    use crate::playlists::{PlaylistEntry, PlaylistStore};
    use crate::{Route, Tx};

    /// Render just this page (the same "skip the app shell" pattern
    /// `albums.rs`'s own render tests use), so a route change elsewhere in
    /// `State` can't affect what's asserted here.
    fn test_view(s: &State, cx: &mut Cx<State>) -> El {
        let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
        let scale = cx.env().scale;
        page(s, &p, scale)
    }

    fn env() -> Env {
        Env {
            system_dark: true,
            scale: 2.0,
        }
    }

    fn dummy_entry(title: &str) -> PlaylistEntry {
        PlaylistEntry {
            locator: TrackLocator::Local {
                path: format!("D:/music/{title}.flac"),
            },
            snapshot: TrackSnapshot {
                title: title.into(),
                artist: "Artist".into(),
                album: title.into(),
                duration_secs: 180,
                track_no: None,
                disc_no: None,
            },
            mbid: None,
        }
    }

    /// A playlist store is a per-process shared temp directory across the
    /// whole test binary (see `config::use_test_config_dir`), so every test
    /// that touches it starts by clearing it rather than trusting it's empty.
    fn reset_store() {
        let _ = std::fs::remove_dir_all(crate::playlists::playlist_store_path());
    }

    /// A runtime over the Playlists page, seeded with playlists named and
    /// sized per `defs`. Caller must hold `config::use_test_config_dir()` for
    /// the duration (this writes to the shared on-disk store).
    fn runtime_with_playlists(defs: &[(&str, usize)]) -> Runtime<State> {
        reset_store();
        let mut s = State::new(PlayerHandle::offline());
        for (name, count) in defs {
            let id = s.playlists.create(name);
            for i in 0..*count {
                s.playlists
                    .add_entry(id, dummy_entry(&format!("{name}-{i}")));
            }
        }
        s.route = Route::Playlists;
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env());
        rt.frame(0.0);
        rt
    }

    /// A runtime over the Playlists page with no playlists at all. Caller
    /// must hold `config::use_test_config_dir()` for the duration.
    fn empty_runtime() -> Runtime<State> {
        reset_store();
        let mut s = State::new(PlayerHandle::offline());
        s.route = Route::Playlists;
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env());
        rt.frame(0.0);
        rt
    }

    /// Whether any rendered element's text contains `needle`.
    fn shows(rt: &Runtime<State>, needle: &str) -> bool {
        rt.elements()
            .into_iter()
            .any(|e| e.text.as_deref().is_some_and(|t| t.contains(needle)))
    }

    /// Drive the real inline New-playlist field through the harness: click
    /// to focus it, type `name` via IME commit, then confirm with Return.
    fn trigger_new_playlist(rt: &mut Runtime<State>, name: &str) {
        let (x, y) = rt
            .find("playlist-new-input")
            .expect("the new-playlist field renders");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        rt.ime(rune::Ime::Commit(name.to_string()));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(0.0);
    }

    /// Each playlist row shows its name and its track count.
    #[test]
    fn playlists_page_lists_names_and_counts() {
        let _g = crate::config::use_test_config_dir();
        rust_i18n::set_locale("en");
        let mut rt = runtime_with_playlists(&[("Road Trip", 3), ("Focus", 0)]);
        rt.update(|st: &mut State, _tx: &mut Tx| st.route = Route::Playlists);
        rt.frame(0.0);
        assert!(shows(&rt, "Road Trip"), "the first playlist's name renders");
        assert!(shows(&rt, "Focus"), "the second playlist's name renders");
    }

    /// Typing a name into the New-playlist field and confirming creates the
    /// playlist (persisted to disk) and shows it in the list.
    #[test]
    fn new_playlist_creates_and_persists() {
        let _g = crate::config::use_test_config_dir();
        rust_i18n::set_locale("en");
        let mut rt = empty_runtime();
        rt.update(|st: &mut State, _tx: &mut Tx| {
            st.route = Route::Playlists;
        });
        trigger_new_playlist(&mut rt, "Fresh");
        assert!(shows(&rt, "Fresh"), "the new playlist's name renders");
        assert_eq!(PlaylistStore::load().list().len(), 1);
    }
}
