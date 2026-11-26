//! Playlists browse: every user-created playlist, as the same responsive card
//! grid the Albums page uses (a square cover, custom image, 2x2 mosaic, or a
//! single cover, the name, and a track count) under the shared browse-page top
//! bar (a bounded search that filters by name, plus a "New playlist" button).
#![forbid(unsafe_code)]

use std::path::Path;
use std::rc::Rc;

use rune::{Color, Cursor, Cx, El, ScrollSpec, col, el, row, text, text_input};

use crate::State;
use crate::playlists::cover::choose_playlist_cover;
use crate::playlists::{PlaylistCover, PlaylistSummary};
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::grid;
use crate::views::player::{cover, cover_from_path};

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
    // Index the memoized per-playlist cover ids (resolved ONCE per library/playlists
    // change) instead of rebuilding a full-catalog locator map per card per frame.
    let cover_ids = s.playlist_cover_ids();
    let resolved: Vec<(u32, String)> = cover_ids
        .get(&summary.id)
        .map(|ids| {
            ids.iter()
                .enumerate()
                .map(|(i, &id)| (id, i.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let custom = summary
        .custom_cover
        .clone()
        .map(|p| p.to_string_lossy().into_owned());
    choose_playlist_cover(custom, &resolved, |id| s.host.art(id).is_some())
}

/// Render a playlist's cover square at `size` pt: the user-set custom cover image
/// for `Custom`, a 2x2 mosaic for `Mosaic`, a single full-bleed cover for
/// `Single`, or the shared "no art" placeholder tile for `Placeholder`. Reuses
/// `views::player::cover`/`cover_from_path` for every tile's pixels so a mosaic
/// tile and a library card's own cover decode identically (same cache, same
/// placeholder); `Custom` falls back to that same placeholder when the file can't
/// be read or hasn't decoded yet, never panicking. Tiles carry no corner radius
/// of their own, the caller clips the whole square to a rounded card shape, so
/// `key_prefix` must be unique per card (the playlist id) so two cards' tiles
/// never collide.
fn playlist_cover_el(
    s: &State,
    which: PlaylistCover,
    size: f32,
    scale: f32,
    p: &Palette,
    key_prefix: &str,
) -> El {
    match which {
        PlaylistCover::Placeholder => {
            cover(s, None, size, 0.0, p, scale).key(format!("{key_prefix}-placeholder"))
        }
        PlaylistCover::Custom(path) => cover_from_path(s, Path::new(&path), size, 0.0, p, scale)
            .key(format!("{key_prefix}-custom")),
        PlaylistCover::Single(id) => {
            cover(s, Some(id), size, 0.0, p, scale).key(format!("{key_prefix}-single"))
        }
        PlaylistCover::Mosaic(ids) => {
            let tile = size / 2.0;
            let mut grid = col()
                .key(format!("{key_prefix}-mosaic"))
                .size(size, size)
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

/// One playlist card, mirroring `albums::album_card`'s shape: a square cover on
/// top (the playlist's custom / mosaic / single art, clipped to a rounded,
/// shadowed tile), the name, then the track-count subtitle. A flex item that wraps
/// at [`grid::MIN_CARD`] and grows to fill its row, like the album/artist cards.
/// Clicking the card opens the playlist's detail page (the same navigation the old
/// row used). Keyed by the playlist's stable id (`playlist-card-{id}`), a playlist
/// has a durable id (unlike an album, keyed by name/slot), so a specific playlist
/// stays addressable across filtering.
fn playlist_card(s: &State, p: &Palette, summary: &PlaylistSummary, card_w: f32, scale: f32) -> El {
    let id = summary.id;
    let which = playlist_cover(s, summary);
    let key_prefix = format!("playlist-cover-{id}");
    let cover_area = el()
        .key(format!("{key_prefix}-area"))
        .size(card_w, card_w)
        .radius(radius::MD)
        // Clip the square (mosaic included) to the rounded card shape, as the
        // album card's cover tile does.
        .clip_rounded(radius::MD)
        .shadow(2.0, 8.0, Color::BLACK.alpha(0.15))
        .child(playlist_cover_el(s, which, card_w, scale, p, &key_prefix));
    col()
        .key(format!("playlist-card-{id}"))
        .role("button")
        .label(summary.name.clone())
        .focusable(true)
        // A flex item like the album/artist cards (see `media_card`): wrap at
        // MIN_CARD, grow to fill the row, capped so a partial last row stays aligned.
        .min_w(grid::MIN_CARD)
        .max_w(card_w + grid::FILL_SLOP)
        .grow(1.0)
        .gap(space::SM)
        .cursor(Cursor::Pointer)
        .child(cover_area)
        .child(
            col()
                .key(format!("playlist-card-text-{id}"))
                .gap(space::XXS)
                .min_w(0.0)
                .child(
                    text(summary.name.clone())
                        .font(type_::CARD_TITLE)
                        .color(p.fg)
                        .truncate(),
                )
                .child(
                    text(t!("playlists.trackCount", count => summary.count).to_string())
                        .font(type_::ITEM)
                        .color(p.text_secondary)
                        .truncate(),
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

/// The New-playlist dialogue's fixed width, in points.
const NEW_PLAYLIST_W: f32 = 340.0;

/// The "New playlist" dialogue (the old app's create flow): a centered modal with
/// a name field + Create/Cancel, opened by the Playlists top bar's New button
/// (`State::playlists_adding`) and mounted in `main.rs`'s overlay layer. `None`
/// while closed. Confirming creates the playlist and jumps into it; Cancel / Esc /
/// an outside press closes. The field + Create keys match the old inline affordance
/// (`playlist-new-input` / `playlist-new-confirm`) so finders/tests keep resolving.
pub fn new_playlist_overlay(s: &State, p: &Palette) -> Option<El> {
    if !s.playlists_adding {
        return None;
    }
    let field = text_input(&s.playlist_new_buf, type_::ITEM)
        .key("playlist-new-input")
        .label(t!("playlists.new").to_string())
        .placeholder(t!("playlists.newPlaceholder").to_string(), p.text_secondary)
        .single_line()
        .w_full()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .color(p.fg)
        .on_edit(|st: &mut State| &mut st.playlist_new_buf)
        .on_submit(|st: &mut State, _tx| confirm_new_playlist(st));
    let footer = row()
        .key("new-playlist-footer")
        .w_full()
        .center_y()
        .justify_end()
        .gap(space::SM)
        .child(
            el().key("new-playlist-cancel")
                .role("button")
                .label(t!("common.cancel").to_string())
                .focusable(true)
                .px(space::MD)
                .py(space::XS)
                .radius(radius::SM)
                .fill(p.muted)
                .cursor(Cursor::Pointer)
                .hover(p.fg.alpha(fx::HOVER))
                .press(p.fg.alpha(fx::PRESS))
                .child(text(t!("common.cancel").to_string()).color(p.fg))
                .on_press(|st: &mut State| st.playlists_adding = false),
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
                .hover(p.bg.alpha(fx::HOVER_FILL))
                .press(p.bg.alpha(fx::PRESS_FILL))
                .child(text(t!("playlists.new").to_string()).color(p.bg))
                .on_press(confirm_new_playlist),
        );
    Some(
        col()
            .key("new-playlist-modal")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .fill(Color::BLACK.alpha(0.45))
            .child(
                col()
                    .key("new-playlist-dialog")
                    .w(NEW_PLAYLIST_W)
                    .fill(p.card)
                    .radius(radius::LG)
                    .border(1.0, p.border)
                    .padding(space::LG)
                    .gap(space::MD)
                    .modal(true, |st: &mut State| st.playlists_adding = false)
                    .child(
                        text(t!("playlists.new").to_string())
                            .font(type_::TITLE)
                            .color(p.fg),
                    )
                    .child(field)
                    .child(footer),
            ),
    )
}

/// The Playlists page: the shared browse-page top bar (a bounded search that
/// filters playlists by name, plus a "New playlist" button) over the same
/// responsive card grid the Albums page uses, one [`playlist_card`] per playlist,
/// or an empty-state message when there are none. Pressing "New playlist"
/// reveals the inline name field under the bar.
/// A playlist card's non-cover height (name + track count): the `virtual_grid`
/// row stride is a `card_w`-square cover mosaic plus this.
const PLAYLIST_CAPTION_H: f32 = 48.0;

pub fn page(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let summaries: Rc<[PlaylistSummary]> = s.playlists.list().into();
    let q = s.playlist_search.text.to_lowercase();
    // Search narrows to a list of INDICES into `summaries`, so the grid can be
    // virtualized (only on-screen cards mount) the same way the Albums grid is.
    let visible: Vec<usize> = summaries
        .iter()
        .enumerate()
        .filter(|(_, pl)| q.is_empty() || pl.name.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect();
    // Responsive grid, the same rule as the Albums grid: read this page's own pane
    // width (first frame has no prior layout → a sensible desktop fallback).
    let pane = cx.layout_size("playlists").map(|[w, _]| w).unwrap_or(820.0);

    let search = crate::views::library::search_box(
        &s.playlist_search,
        "playlists-search",
        t!("playlists.searchPlaceholder", count => summaries.len()).to_string(),
        p,
        |st: &mut State| &mut st.playlist_search,
    );
    // The "New playlist" affordance in the bar's trailing slot (mirrors the Albums
    // Filters button). Lit while the inline name field is open; pressing it toggles
    // that field in/out under the bar.
    let adding = s.playlists_adding;
    let new_btn = row()
        .key("playlists-new")
        .role("button")
        .label(t!("playlists.new").to_string())
        .focusable(true)
        .center_y()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if adding { p.accent.alpha(0.2) } else { p.muted })
        .hover(p.fg.alpha(fx::HOVER))
        .child(
            text(t!("playlists.new").to_string())
                .font(type_::ITEM)
                .color(if adding { p.accent } else { p.fg }),
        )
        .on_press(|st: &mut State| st.playlists_adding = !st.playlists_adding);
    let search_bar = crate::views::library::page_search_bar("playlists", search, Some(new_btn));

    // The body: the empty-state message when there are no playlists at all, else
    // the responsive grid of cover cards (filtered by the search), mirroring the
    // Albums grid's structure exactly.
    let body: El = if summaries.is_empty() {
        col().key("playlists-empty").padding(space::PANE).child(
            text(t!("playlists.empty").to_string())
                .font(type_::ITEM)
                .color(p.text_secondary),
        )
    } else {
        // Virtualized grid: only the on-screen window of cards is built. The closure
        // captures the summaries + visible index list by `Rc`.
        let sums = summaries.clone();
        let visible: Rc<[usize]> = visible.into();
        let count = visible.len();
        let vis_cards = visible.clone();
        let pal = *p;
        grid::virtual_grid(
            "playlists-grid",
            pane,
            count,
            1.0, // square 2x2 cover mosaic
            PLAYLIST_CAPTION_H,
            Rc::new(move |s: &State, pos: usize, card_w: f32| {
                playlist_card(s, &pal, &sums[vis_cards[pos]], card_w, scale)
            }),
        )
    };

    let page = col()
        .key("playlists")
        .grow(1.0)
        .min_h(0.0)
        .child(search_bar);
    // The New-playlist field now lives in a centered modal ([`new_playlist_overlay`],
    // mounted in `main.rs`'s overlay layer) rather than dropping in under the bar.
    page.child(
        // Scroll on the growing wrapper, not the grid, see `albums_page`.
        col()
            .key("playlists-scroll")
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .scroll_edge_fade(24.0)
            .grow(1.0)
            .min_h(0.0)
            .child(body),
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
        // Mirror `main.rs`'s view: the page plus the New-playlist modal overlay,
        // so a test can reach the modal's field once "New playlist" opens it.
        let mut root = col().key("test-root").child(page(s, &p, cx, scale));
        if let Some(overlay) = new_playlist_overlay(s, &p) {
            root = root.child(overlay);
        }
        root
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

    /// Start from an empty playlist store. The test thread's config dir already
    /// starts empty (see `config::soul_player_dir`); this keeps a test explicit.
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

    /// Drive the real inline New-playlist field through the harness: reveal it
    /// via the top bar's "New playlist" button, click to focus it, type `name`
    /// via IME commit, then confirm with Return.
    fn trigger_new_playlist(rt: &mut Runtime<State>, name: &str) {
        let (bx, by) = rt
            .find("playlists-new")
            .expect("the New playlist button renders");
        rt.pointer_down(bx, by);
        rt.pointer_up(bx, by);
        rt.frame(0.0);
        // The field drops in animated from opacity 0; settle so it is hittable +
        // focusable before we click into it (else the IME commit goes nowhere).
        rt.settle_all();
        rt.frame(0.0);
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

    /// `State::playlist_cover_ids` resolves every playlist's cover ONCE and memoizes
    /// it by (library fingerprint, playlists fingerprint): repeated calls return the
    /// SAME `Rc` (a cache hit, not the old per-card/per-frame `resolve_locators`
    /// rebuild), and adding a playlist invalidates it.
    #[test]
    fn playlist_cover_ids_memoizes_and_invalidates() {
        let _g = crate::config::use_test_config_dir();
        reset_store();
        let mut s = State::new(PlayerHandle::offline());
        s.playlists.create("A");
        s.playlists.create("B");

        let first = s.playlist_cover_ids();
        let second = s.playlist_cover_ids();
        assert!(
            std::rc::Rc::ptr_eq(&first, &second),
            "same inputs return the same Rc (a cache hit, not a rebuild)"
        );
        assert_eq!(first.len(), 2, "one entry per playlist");

        s.playlists.create("C");
        let third = s.playlist_cover_ids();
        assert!(
            !std::rc::Rc::ptr_eq(&first, &third),
            "adding a playlist invalidates the memo"
        );
        assert_eq!(third.len(), 3);
    }

    /// Each playlist row shows its name and its track count.
    #[test]
    fn playlists_page_lists_names_and_counts() {
        let _locale = crate::config::use_test_locale("en");
        let _g = crate::config::use_test_config_dir();
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
        let _locale = crate::config::use_test_locale("en");
        let _g = crate::config::use_test_config_dir();
        let mut rt = empty_runtime();
        rt.update(|st: &mut State, _tx: &mut Tx| {
            st.route = Route::Playlists;
        });
        trigger_new_playlist(&mut rt, "Fresh");
        assert!(shows(&rt, "Fresh"), "the new playlist's name renders");
        assert_eq!(PlaylistStore::load().list().len(), 1);
    }

    /// The labelled, focusable cards in the Playlists grid (one per playlist), by
    /// their stable key prefix + interactive role (excludes the inner text col).
    fn grid_cards(rt: &Runtime<State>) -> Vec<rune::ElementInfo> {
        rt.elements()
            .into_iter()
            .filter(|e| !e.exiting)
            .filter(|e| {
                e.key
                    .as_deref()
                    .is_some_and(|k| k.starts_with("playlist-card-"))
                    && e.role == "button"
            })
            .collect()
    }

    /// The Playlists page renders the same responsive card GRID the Albums page
    /// does (role "grid"), not a row list: one focusable, labelled card per
    /// playlist, which the a11y sweep also depends on.
    #[test]
    fn playlists_page_renders_a_grid_of_cards() {
        let _locale = crate::config::use_test_locale("en");
        let _g = crate::config::use_test_config_dir();
        let rt = runtime_with_playlists(&[("Road Trip", 3), ("Focus", 0)]);
        let grid = rt
            .elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some("playlists-grid"))
            .expect("the page renders a grid container");
        assert_eq!(grid.role, "grid", "the container carries the grid role");

        let cards = grid_cards(&rt);
        assert_eq!(cards.len(), 2, "one card per playlist");
        assert!(
            cards.iter().all(|e| e.focusable && e.role == "button"),
            "each card is a focusable button"
        );
        assert!(
            cards
                .iter()
                .any(|e| e.label.as_deref() == Some("Road Trip"))
        );
        assert!(cards.iter().any(|e| e.label.as_deref() == Some("Focus")));
    }

    /// The Playlists search filters the grid by playlist name (empty query =
    /// all), matching the Albums/Artists/Genres grids.
    #[test]
    fn playlists_search_narrows_the_grid() {
        let _locale = crate::config::use_test_locale("en");
        let _g = crate::config::use_test_config_dir();
        let mut rt = runtime_with_playlists(&[("Road Trip", 3), ("Focus", 0)]);
        assert!(
            rt.find("playlists-search").is_some(),
            "the search box renders above the grid"
        );
        let labels = |rt: &Runtime<State>| -> Vec<String> {
            grid_cards(rt).into_iter().filter_map(|e| e.label).collect()
        };
        assert_eq!(labels(&rt).len(), 2, "both playlists show with no query");

        rt.update(|st: &mut State, _tx: &mut Tx| st.playlist_search = rune::TextEdit::new("road"));
        rt.frame(0.0);
        let got = labels(&rt);
        assert!(
            got.contains(&"Road Trip".to_string()),
            "the name match stays, got {got:?}"
        );
        assert!(
            !got.contains(&"Focus".to_string()),
            "the non-match is filtered out, got {got:?}"
        );

        rt.update(|st: &mut State, _tx: &mut Tx| st.playlist_search = rune::TextEdit::new(""));
        rt.frame(0.0);
        assert_eq!(
            labels(&rt).len(),
            2,
            "clearing the query shows every playlist again"
        );
    }
}
