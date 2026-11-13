//! Main-area track list: one row per library track, click to play. A search bar
//! filters the list by title/artist/album (typed via the key handler — `/` to
//! start — since Rune has no text widget yet).
#![forbid(unsafe_code)]

use rune::{
    Cursor, Cx, El, PointerPress, PopupItem, PopupMenu, ScrollSpec, Tx, col, el, icon, row, text,
};
use soul_player_host::{BrowseAxis, Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::player::cover;

pub(crate) fn fmt_dur(secs: f64) -> String {
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
fn track_row(
    s: &State,
    p: &Palette,
    t: &TrackView,
    ids: &[u32],
    index: usize,
    compact: bool,
    scale: f32,
) -> El {
    let playing = s.now_playing_id == Some(t.id);
    let ids = ids.to_vec();
    let start = index as u32;
    let tid = t.id; // Copy out of the borrow so the button/menu closures can own it.
    let menu_ids = ids.clone(); // the context menu's "Play" needs its own copy.
    let artist = t.artist.clone().unwrap_or_default();

    let title = text(
        t.title
            .clone()
            .unwrap_or_else(|| t!("library.unknown").to_string()),
    )
    .font(type_::ITEM)
    .color(if playing { p.accent } else { p.fg })
    .truncate();
    // Compact (narrow pane) drops the artist/album columns — the old app's
    // TrackList hides them below `sm` — so the artist rides under the title as a
    // subtitle instead of disappearing. Wide keeps the title a single line.
    let title_block: El = if compact && !artist.is_empty() {
        col()
            .key("track-titleblock")
            .gap(space::XXS)
            .min_w(0.0)
            .grow(1.0)
            .child(title)
            .child(
                text(artist.clone())
                    .font(type_::CAPTION)
                    .color(p.text_secondary)
                    .truncate(),
            )
    } else {
        title.grow(1.0)
    };
    // Cover + title hug each other, mirroring the old app's `flex items-center`
    // title column with its `w-8 h-8 rounded-lg` (32 px) thumbnail. The pair
    // grows to fill the title column; `min_w(0)` lets the title truncate.
    let title_col = row()
        .key("track-title")
        .gap(space::SM)
        .center_y()
        .grow(1.0)
        .min_w(0.0)
        .child(el().key(format!("track-cover-{}", t.id)).child(cover(
            s,
            Some(t.id),
            32.0,
            radius::MD,
            p,
            scale,
        )))
        .child(title_block);

    let mut r = row()
        .key(format!("track-{}", t.id))
        .gap(space::MD)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS));
    if playing {
        // The now-playing row keeps a persistent accent wash, as the old app's
        // TrackList `isCurrentTrack ? bg-accent/20`.
        r = r.fill(p.accent.alpha(0.2));
    }
    r = r
        .child(
            // The now-playing row shows a play/pause state glyph in the accent
            // colour (⏸ while playing — click the row to pause; ▶ when paused);
            // other rows show their track number. The old app's TrackList swaps
            // the index for a play/pause button on the current/hovered row.
            if playing {
                text(if s.playing { "⏸" } else { "▶" }).color(p.accent)
            } else {
                text(format!("{}", index + 1)).color(p.text_secondary)
            }
            .font(type_::ITEM)
            .w(28.0),
        )
        .child(title_col);
    // The standard layout keeps the artist and album as their own columns; the
    // compact layout dropped them above (artist moved under the title).
    if !compact {
        r = r
            .child(
                text(artist)
                    .font(type_::ITEM)
                    .color(p.text_secondary)
                    .truncate()
                    .grow(1.0),
            )
            .child(
                text(t.album.clone().unwrap_or_default())
                    .font(type_::ITEM)
                    .color(p.text_secondary)
                    .truncate()
                    .grow(1.0),
            );
    }
    r.child(
        text(fmt_dur(t.duration_secs))
            .font(type_::ITEM)
            .color(p.text_secondary)
            .w(56.0),
    )
    // Add-to-queue (`+`): appends the track to the override queue, which
    // shows up in the "Up Next" sidebar. Nested in the clickable row, but
    // Rune hit-tests to the innermost target, so pressing it enqueues
    // without also starting playback.
    .child(
        el().key(format!("enqueue-{}", t.id))
            .padding(space::SNUG)
            .radius(radius::SM)
            .fill(p.muted)
            .cursor(Cursor::Pointer)
            .hover(p.fg.alpha(fx::HOVER))
            .press(p.fg.alpha(fx::PRESS))
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
        // Clicking the row plays that track, or toggles pause/resume when it is
        // already the one playing — so re-clicking the current row pauses rather
        // than restarting it.
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
        .gap(space::SM)
        .padding(space::SM)
        .center_y()
        .child(icon(&icons::SEARCH, 16.0).color(p.text_secondary))
        .child(
            el().key("search-field")
                .grow(1.0)
                .padding(space::SNUG)
                .radius(radius::SM)
                .fill(p.muted)
                .cursor(Cursor::Pointer)
                .hover(p.fg.alpha(fx::HOVER))
                .press(p.fg.alpha(fx::PRESS))
                .child(text(shown).color(if dim { p.text_secondary } else { p.fg }))
                .on_press(|st: &mut State| st.searching = true),
        );
    if !s.search.is_empty() {
        bar = bar.child(
            el().key("search-clear")
                .padding(space::SNUG)
                .radius(radius::SM)
                .fill(p.muted)
                .cursor(Cursor::Pointer)
                .hover(p.fg.alpha(fx::HOVER))
                .press(p.fg.alpha(fx::PRESS))
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
pub fn track_list(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    // Respond to the content pane's own width (window − sidebar − paddings), read
    // from last frame's layout — not the window — so a wide sidebar also triggers
    // the compact rows. Below the old app's `sm` (640) the artist/album columns
    // collapse into a subtitle. First frame has no prior layout → assume standard.
    let compact = cx
        .layout_size("library-page")
        .map(|[w, _]| w <= 640.0)
        .unwrap_or(false);
    if s.library.is_empty() {
        // Cold launch with no SOUL_PLAYER_FOLDER: a usable entry point rather
        // than a blank list.
        return col()
            .key("empty")
            .w_full()
            .h_full()
            .center()
            .gap(space::MD)
            .child(
                text(t!("library.noMusic").to_string())
                    .font(type_::TITLE)
                    .color(p.fg),
            )
            .child(text(t!("library.noMusicHint").to_string()).color(p.text_secondary))
            .child(
                el().key("go-to-sources")
                    .padding(space::MD)
                    .radius(radius::MD)
                    .fill(p.primary)
                    .center()
                    .cursor(Cursor::Pointer)
                    .hover(p.bg.alpha(fx::HOVER_FILL))
                    .press(p.bg.alpha(fx::PRESS_FILL))
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
        let mut list = col().key("tracks").gap(space::XXS);
        for (i, t) in visible.iter().enumerate() {
            list = list.child(track_row(s, p, t, &ids, i, compact, scale));
        }
        list.scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .min_h(0.0)
    };
    col()
        .key("library-page")
        .grow(1.0)
        .min_h(0.0)
        .child(search_bar(s, p))
        .child(sort_bar(s, p))
        .child(body)
}

/// One sort-order choice; the active axis is filled with the primary color.
fn sort_button(label: String, key: &str, axis: BrowseAxis, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .padding(space::SNUG)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .hover(if active {
            p.bg.alpha(fx::HOVER_FILL)
        } else {
            p.fg.alpha(fx::HOVER)
        })
        .press(if active {
            p.bg.alpha(fx::PRESS_FILL)
        } else {
            p.fg.alpha(fx::PRESS)
        })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| crate::set_sort(s, axis))
}

/// The sort bar above the track list: switch which baked axis orders the library.
fn sort_bar(s: &State, p: &Palette) -> El {
    let a = s.sort_axis;
    row()
        .key("sort-bar")
        .gap(space::SM)
        .padding(space::SM)
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
