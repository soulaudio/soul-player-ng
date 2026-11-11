//! Main-area track list: one row per library track, click to play. A search bar
//! filters the list by title/artist/album (typed via the key handler — `/` to
//! start — since Rune has no text widget yet).
#![forbid(unsafe_code)]

use rune::tok::type_;
use rune::{
    Cursor, El, PointerPress, PopupItem, PopupMenu, ScrollSpec, Tx, col, el, icon, row, text,
};
use soul_player_host::{BrowseAxis, Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::views::icons;

fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Case-insensitive substring match over the fields a user would search by.
/// `q` is already lowercased by the caller.
fn track_matches(t: &TrackView, q: &str) -> bool {
    let hit = |o: &Option<String>| o.as_deref().is_some_and(|v| v.to_lowercase().contains(q));
    hit(&t.title) || hit(&t.artist) || hit(&t.album)
}

/// One track row. `ids` is the id list of the *currently visible* (possibly
/// filtered) tracks, so playing starts a context over exactly what's on screen;
/// `index` is the row's position within it.
fn track_row(s: &State, p: &Palette, t: &TrackView, ids: &[u32], index: usize) -> El {
    let playing = s.now_playing_id == Some(t.id);
    let ids = ids.to_vec();
    let start = index as u32;
    let tid = t.id; // Copy out of the borrow so the button/menu closures can own it.
    let menu_ids = ids.clone(); // the context menu's "Play" needs its own copy.
    row()
        .key(format!("track-{}", t.id))
        .gap(12.0)
        .padding(8.0)
        .radius(6.0)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(0.06))
        .child(
            if playing {
                // ▶/⏸ are already proven-good glyphs in this font (the
                // transport's play/pause button uses the same pair), so the
                // now-playing row swaps its index for one of them instead of
                // introducing an unverified glyph.
                text(if s.playing { "▶" } else { "⏸" }).color(p.accent)
            } else {
                text(format!("{}", index + 1)).color(p.text_secondary)
            }
            .w(28.0),
        )
        .child(
            text(
                t.title
                    .clone()
                    .unwrap_or_else(|| t!("library.unknown").to_string()),
            )
            .color(if playing { p.accent } else { p.fg })
            .grow(1.0),
        )
        .child(
            text(t.artist.clone().unwrap_or_default())
                .color(p.text_secondary)
                .grow(1.0),
        )
        .child(
            text(t.album.clone().unwrap_or_default())
                .color(p.text_secondary)
                .grow(1.0),
        )
        .child(
            text(fmt_dur(t.duration_secs))
                .color(p.text_secondary)
                .w(56.0),
        )
        // Add-to-queue (`+`): appends the track to the override queue, which
        // shows up in the "Up Next" sidebar. Nested in the clickable row, but
        // Rune hit-tests to the innermost target, so pressing it enqueues
        // without also starting playback.
        .child(
            el().key(format!("enqueue-{}", t.id))
                .padding(6.0)
                .radius(6.0)
                .fill(p.muted)
                .cursor(Cursor::Pointer)
                .child(icon(&icons::PLUS, 14.0).color(p.text_secondary))
                .on_press(move |st: &mut State| st.host.send(Command::Enqueue(tid))),
        )
        // Right-click: a context menu with the fuller per-track actions (Play,
        // Play next, Add to queue). Anchored at the click point. Where popup
        // menus aren't supported, the inline + button remains the fallback.
        .on_secondary_press(move |_st: &mut State, press: PointerPress, tx: &mut Tx| {
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
                    )),
                press,
                move |st: &mut State, picked, _tx: &mut Tx| match picked {
                    Some("play") => st.host.send(Command::PlayContext {
                        ids: play_ids.clone(),
                        start,
                    }),
                    Some("play-next") => st.host.send(Command::PlayNext(tid)),
                    Some("enqueue") => st.host.send(Command::Enqueue(tid)),
                    _ => {}
                },
            );
        })
        .on_press(move |st: &mut State| {
            st.host.send(Command::PlayContext {
                ids: ids.clone(),
                start,
            })
        })
}

/// The search bar: shows the current filter and, while active, a cursor. Click
/// it (or press `/`) to start typing; the ✕ clears it. No focused text field —
/// `main::handle_key` routes keystrokes into `State::search`.
fn search_bar(s: &State, p: &Palette) -> El {
    let (shown, dim) = if s.search.is_empty() && !s.searching {
        (t!("library.searchPlaceholder").to_string(), true)
    } else if s.searching {
        (format!("{}│", s.search), false)
    } else {
        (s.search.clone(), false)
    };
    let mut bar = row()
        .key("search-bar")
        .gap(8.0)
        .padding(8.0)
        .center_y()
        .child(icon(&icons::SEARCH, 16.0).color(p.text_secondary))
        .child(
            el().key("search-field")
                .grow(1.0)
                .padding(6.0)
                .radius(6.0)
                .fill(p.muted)
                .cursor(Cursor::Pointer)
                .child(text(shown).color(if dim { p.text_secondary } else { p.fg }))
                .on_press(|st: &mut State| st.searching = true),
        );
    if !s.search.is_empty() {
        bar = bar.child(
            el().key("search-clear")
                .padding(6.0)
                .radius(6.0)
                .fill(p.muted)
                .cursor(Cursor::Pointer)
                .child(text("✕").color(p.text_secondary))
                .on_press(|st: &mut State| {
                    st.search.clear();
                    st.searching = false;
                }),
        );
    }
    bar
}

/// The library's ~38-track testdata set fits comfortably in a plain scrolled
/// column (`.scroll`); `El::virtualize`/`virtual_rows` (see
/// `rune::el::virtual_rows`) is the path to reach for once a real-world
/// library runs into the hundreds or thousands of tracks, since it only lays
/// out/shapes text for the rows in (and just around) the viewport.
///
/// `.grow(1.0).min_h(0.0)` make the viewport take its bounded parent's full
/// remaining height (rather than its content's natural height) so it scrolls
/// instead of overflowing — see `El::scroll`'s doc comment.
pub fn track_list(s: &State, p: &Palette) -> El {
    if s.library.is_empty() {
        // Cold launch with no SOUL_PLAYER_FOLDER: a usable entry point rather
        // than a blank list.
        return col()
            .key("empty")
            .w_full()
            .h_full()
            .center()
            .gap(12.0)
            .child(
                text(t!("library.noMusic").to_string())
                    .font(type_::TITLE)
                    .color(p.fg),
            )
            .child(text(t!("library.noMusicHint").to_string()).color(p.text_secondary))
            .child(
                el().key("go-to-sources")
                    .padding(12.0)
                    .radius(8.0)
                    .fill(p.primary)
                    .center()
                    .cursor(Cursor::Pointer)
                    .child(text(t!("library.addSource").to_string()).color(p.bg))
                    .on_press(|s: &mut State| s.route = crate::Route::Settings),
            );
    }
    let q = s.search.to_lowercase();
    let visible: Vec<&TrackView> = if q.is_empty() {
        s.library.iter().collect()
    } else {
        s.library.iter().filter(|t| track_matches(t, &q)).collect()
    };
    let body = if visible.is_empty() {
        col().key("no-matches").grow(1.0).min_h(0.0).center().child(
            text(t!("library.noMatch", query => s.search).to_string()).color(p.text_secondary),
        )
    } else {
        let ids: Vec<u32> = visible.iter().map(|t| t.id).collect();
        let mut list = col().key("tracks").gap(2.0);
        for (i, t) in visible.iter().enumerate() {
            list = list.child(track_row(s, p, t, &ids, i));
        }
        list.scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
    };
    col()
        .grow(1.0)
        .min_h(0.0)
        .child(search_bar(s, p))
        .child(sort_bar(s, p))
        .child(body)
}

/// One sort-order choice; the active axis is filled with the primary color.
fn sort_button(label: String, key: &str, axis: BrowseAxis, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .padding(6.0)
        .radius(6.0)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| crate::set_sort(s, axis))
}

/// The sort bar above the track list: switch which baked axis orders the library.
fn sort_bar(s: &State, p: &Palette) -> El {
    let a = s.sort_axis;
    row()
        .key("sort-bar")
        .gap(8.0)
        .padding(8.0)
        .child(text(t!("library.sort").to_string()).color(p.text_secondary))
        .child(sort_button(
            t!("library.sortArtist").to_string(),
            "sort-artist",
            BrowseAxis::ArtistAlbumTrack,
            a == BrowseAxis::ArtistAlbumTrack,
            p,
        ))
        .child(sort_button(
            t!("library.sortAlbum").to_string(),
            "sort-album",
            BrowseAxis::Album,
            a == BrowseAxis::Album,
            p,
        ))
        .child(sort_button(
            t!("library.sortTitle").to_string(),
            "sort-title",
            BrowseAxis::Title,
            a == BrowseAxis::Title,
            p,
        ))
        .child(sort_button(
            t!("library.sortFolder").to_string(),
            "sort-folder",
            BrowseAxis::Folder,
            a == BrowseAxis::Folder,
            p,
        ))
}
