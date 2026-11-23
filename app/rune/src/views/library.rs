//! Main-area track list: one row per library track, click to play. A search bar
//! filters the list by title/artist/album (typed via the key handler — `/` to
//! start — since Rune has no text widget yet).
#![forbid(unsafe_code)]

use rune::{
    Align, Color, Cursor, Cx, El, Live, PointerPress, PopupItem, PopupMenu, ScrollSpec, Tx, col,
    el, icon, row, text,
};
use soul_player_host::{BrowseAxis, Command, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::player::{cover, remote_badge};
use crate::views::scrollbar;

thread_local! {
    /// Per-render-thread thumb geometry for the track-list scroll viewport.
    static LIB_SCROLL_THUMB: Live<[f32; 4]> = Live::new([0.0; 4]);
}

pub(crate) fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// "1 album" / "N albums" — picks the singular or plural form in code, since
/// rust-i18n 3 has no CLDR plurals (two forms suffice for en/de/es/cs; zh/ja are
/// plural-invariant). Avoids the "1 albums" grammar slip the old app doesn't have.
pub(crate) fn albums_label(n: usize) -> String {
    if n == 1 {
        t!("count.albumOne").to_string()
    } else {
        t!("count.albumOther", count => n).to_string()
    }
}

/// "1 track" / "N tracks" — see [`albums_label`].
pub(crate) fn tracks_label(n: usize) -> String {
    if n == 1 {
        t!("count.trackOne").to_string()
    } else {
        t!("count.trackOther", count => n).to_string()
    }
}

/// A list-filter search field matching the old app's: a magnifier icon then the
/// typed text / placeholder, inside a muted rounded box. The `input_key` stays
/// on the inner `text_input` so existing finders and tests keep resolving it;
/// the box row is keyed `{input_key}-box`. The caller sizes the box (`.grow(1)`
/// beside a Filters button, or `.w_full()` on its own).
pub(crate) fn search_box(
    edit: &rune::TextEdit,
    input_key: &'static str,
    placeholder: String,
    p: &Palette,
    on_edit: impl Fn(&mut State) -> &mut rune::TextEdit + 'static,
) -> El {
    row()
        .key(format!("{input_key}-box"))
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .child(icon(&icons::SEARCH, 16.0).color(p.text_secondary))
        .child(
            rune::text_input(edit, type_::ITEM)
                .key(input_key)
                .single_line()
                .label(placeholder.clone())
                .placeholder(placeholder, p.text_secondary)
                .grow(1.0)
                .color(p.fg)
                .on_edit(on_edit),
        )
}

/// How wide the browse-page search may grow. The search is a *bounded* field, not
/// a full-pane bar: it grows with the pane up to this cap, then stops (and shrinks
/// below it on a narrow pane), so it stays comfortable to read on a wide window
/// rather than stretching edge to edge.
pub(crate) const SEARCH_MAX_W: f32 = 360.0;

/// The shared browse-page top bar, reused by Albums / Artists / Genres / Tracks /
/// Playlists so every list page opens with the same search in the same place. A
/// left-aligned `search` field bounded to [`SEARCH_MAX_W`] (responsive — it grows
/// with the pane up to that cap, not the full width), plus optional `trailing`
/// controls (e.g. the Albums Filters button, the Playlists New button) pinned to
/// the right edge by a growing spacer between them. Keyed `{page}-search-bar`, so
/// each page's existing finders/tests (`albums-search-bar`, …) keep resolving.
pub(crate) fn page_search_bar(page: &str, search: El, trailing: Option<El>) -> El {
    let mut bar = row()
        .key(format!("{page}-search-bar"))
        .px(space::PANE)
        .pt(space::SM)
        .gap(space::SM)
        .center_y()
        .child(search.grow(1.0).max_w(SEARCH_MAX_W));
    if let Some(trailing) = trailing {
        // A growing spacer pushes the trailing control to the pane's right edge,
        // so the search stays left-aligned and bounded regardless of pane width.
        bar = bar
            .child(el().key(format!("{page}-search-spacer")).grow(1.0))
            .child(trailing);
    }
    bar
}

/// The Integrated-LUFS cell for a track row: `-14.0`, or `·` until the track is analyzed
/// (or if it is below the measurement gate / silent).
fn lufs_cell(t: &TrackView, p: &Palette) -> El {
    let label = match t.loudness {
        Some(l) if l.integrated_lufs.is_finite() => format!("{:.1}", l.integrated_lufs),
        _ => "·".to_string(),
    };
    text(label)
        .key(format!("lufs-{}", t.id))
        .font(type_::ITEM)
        .color(p.text_secondary)
        .w(64.0)
}

/// The DR cell for a track row: `DR12`, color-banded by [`crate::views::signal_path::dr_color`]
/// (green dynamic, warm compressed), or `·` until analyzed. DR is an indicator, not a precise
/// quality score (it is crest factor, not true dynamic range).
fn dr_cell(t: &TrackView, p: &Palette) -> El {
    let (label, color) = match t.loudness {
        Some(l) => (
            format!("DR{}", l.dr.round() as i32),
            crate::views::signal_path::dr_color(l.dr, p),
        ),
        None => ("·".to_string(), p.text_secondary),
    };
    text(label)
        .key(format!("dr-{}", t.id))
        .font(type_::ITEM)
        .color(color)
        .w(44.0)
}

/// The FORMAT badge's codec-family color: lossless containers (FLAC/WAV/AIFF/
/// ALAC/WV) get the palette's dedicated blue/teal accent, DSD (DSF/DFF) gets
/// its dedicated purple/magenta accent, and everything else (MP3/AAC/OGG/OPUS/
/// M4A/MP1/MP2/…) reads as the neutral "lossy" grey, mirroring the old app's
/// FORMAT column, which colors M4A the same grey as MP3 (the container alone
/// can't say whether it holds lossy AAC or lossless ALAC).
fn format_family_color(format: &str, p: &Palette) -> Color {
    match format {
        "FLAC" | "WAV" | "AIFF" | "ALAC" | "WV" => p.format_lossless,
        "DSF" | "DFF" => p.format_dsd,
        _ => p.text_secondary,
    }
}

/// A compact, color-coded pill for a track's codec/container label (`"FLAC"`,
/// `"MP3"`, `"DSF"`, …): the Tracks table's FORMAT column, mirroring the old
/// app's per-row format badge (see [`format_family_color`]). Kept subtle: a
/// low-alpha tinted fill behind small, mono-leaning text in the same hue at
/// full saturation, so it reads as a quiet tag rather than competing with the
/// row's title. The caller wraps this in its own uniquely-keyed element (the
/// same convention [`crate::views::player::cover`] uses) since the label alone
/// repeats across rows.
pub(crate) fn format_badge(format: &str, p: &Palette) -> El {
    let c = format_family_color(format, p);
    el().key("format-badge")
        .px(space::XS)
        .py(space::XXS)
        .radius(radius::SM)
        .fill(c.alpha(0.16))
        .child(
            text(format.to_string())
                .font(type_::CAPTION.mono())
                .color(c),
        )
}

/// Case-insensitive substring match over the fields a user would search by.
/// `q` is already lowercased by the caller.
pub(crate) fn track_matches(t: &TrackView, q: &str) -> bool {
    let hit = |o: &Option<String>| o.as_deref().is_some_and(|v| v.to_lowercase().contains(q));
    hit(&t.title) || hit(&t.artist) || hit(&t.album)
}

/// How much horizontal room the track list has, in three tiers driven by the
/// content pane's own width (see `track_list`). Each tier reveals one more
/// column band: `Compact` folds artist under the title; `Standard` breaks out
/// the artist/album columns; `Wide` additionally shows the loudness/DR columns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowWidth {
    Compact,
    Standard,
    Wide,
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
    width: RowWidth,
    scale: f32,
) -> El {
    let compact = width == RowWidth::Compact;
    let playing = s.now_playing_id == Some(t.id);
    let ids = ids.to_vec();
    let start = index as u32;
    let tid = t.id; // Copy out of the borrow so the button/menu closures can own it.
    let menu_ids = ids.clone(); // the context menu's "Play" needs its own copy.
    let artist = t.artist.clone().unwrap_or_default();
    // Accessible name for the whole row (keyboard focus / screen readers).
    let row_label = {
        let ti = t
            .title
            .clone()
            .unwrap_or_else(|| t!("library.unknown").to_string());
        if artist.is_empty() {
            ti
        } else {
            format!("{ti} — {artist}")
        }
    };

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
        title.grow(1.0).min_w(0.0)
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
        .role("button")
        .label(row_label)
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
            .align(Align::Center)
            .w(28.0),
        )
        .child(title_col);
    // A subtle cloud badge for a track streaming from a remote source;
    // nothing renders for a local track, the unremarked default.
    if let Some(host) = &t.remote_source {
        r = r.child(remote_badge(t.id, host, p));
    }
    // The standard layout keeps the artist and album as their own columns; the
    // compact layout dropped them above (artist moved under the title).
    if !compact {
        r = r
            .child(
                text(artist)
                    .font(type_::ITEM)
                    .color(p.text_secondary)
                    .truncate()
                    .grow(1.0)
                    .min_w(0.0),
            )
            .child(
                text(t.album.clone().unwrap_or_default())
                    .font(type_::ITEM)
                    .color(p.text_secondary)
                    .truncate()
                    .grow(1.0)
                    .min_w(0.0),
            );
    }
    // The FORMAT badge, mirroring the old app's column order (artist/album,
    // then FORMAT, then duration). Like Integrated LUFS, it only joins on a
    // wide pane: the standard tier's artist/album/duration/DR already fill its
    // width budget (a narrower sidebar panel, e.g. the Up Next queue, can pinch
    // a `> 760` content pane back below that threshold).
    if width == RowWidth::Wide
        && let Some(fmt) = &t.format
    {
        r = r.child(
            el().key(format!("format-badge-{}", t.id))
                .w(52.0)
                .child(format_badge(fmt, p)),
        );
    }
    r = r.child(
        text(fmt_dur(t.duration_secs))
            .font(type_::ITEM)
            .color(p.text_secondary)
            .w(56.0),
    );
    // Audiophile loudness columns, mirroring the duration column (right-aligned, fixed
    // width). The whole pair is behind the Appearance "loudness columns" preference
    // (`show_loudness_columns`). When shown: DR is the headline indicator (narrow,
    // color-banded) at every width; Integrated LUFS is secondary and only joins on a
    // WIDE pane, where there is room beside artist/album without crowding them. Each
    // shows "·" until the background sweep analyzes the track.
    if s.show_loudness_columns {
        r = r.child(dr_cell(t, p));
        if width == RowWidth::Wide {
            r = r.child(lufs_cell(t, p));
        }
    }
    r
        // Add-to-queue (`+`): appends the track to the override queue, which
        // shows up in the "Up Next" sidebar. Nested in the clickable row, but
        // Rune hit-tests to the innermost target, so pressing it enqueues
        // without also starting playback.
        .child(
            el().key(format!("enqueue-{}", t.id))
                .role("button")
                .label(t!("library.addToQueue").to_string())
                .focusable(true)
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
        // Play next, Add to queue, Add to playlist). Anchored at the click
        // point. "Add to playlist" is a native menu ITEM opening the themed
        // `views::add_to_playlist` overlay (not a `PopupMenu::picker`, which the
        // arch test bans only for selection dropdowns). Where popup menus aren't
        // supported, the inline + button remains the add-to-queue fallback.
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

/// Uppercase column-header labels over the Tracks table (the old app's
/// TITLE/ARTIST/ALBUM/FORMAT/DURATION row), mirroring [`track_row`]'s own
/// per-tier column layout *exactly* (same `RowWidth` gating, same per-column
/// `.w(..)`/`.grow(1.0)`), so each label sits directly over its row's cells at
/// every pane width. Purely decorative text, so it carries no role/label of
/// its own; the leading `#`, the cover-thumbnail gap and the trailing
/// enqueue-button gap are blank spacers sized to match their row counterpart.
fn track_header_row(p: &Palette, width: RowWidth, show_loudness: bool) -> El {
    let compact = width == RowWidth::Compact;
    let label = |s: String| text(s).font(type_::CAPTION).color(p.text_secondary);

    // The title header hugs a blank spacer the width of the row's cover
    // thumbnail, same gap, so TITLE lands over the row's title text rather
    // than over its cover.
    let title_header = row()
        .key("tracks-header-title")
        .gap(space::SM)
        .center_y()
        .grow(1.0)
        .min_w(0.0)
        .child(el().key("tracks-header-cover-spacer").size(32.0, 32.0))
        .child(
            label(t!("library.colTitle").to_uppercase())
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        );

    let mut r = row()
        .key("tracks-header")
        .gap(space::MD)
        .center_y()
        .px(space::MD)
        .py(space::XS)
        .child(el().key("tracks-header-index").w(28.0))
        .child(title_header);
    if !compact {
        r = r
            .child(
                label(t!("library.colArtist").to_uppercase())
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate(),
            )
            .child(
                label(t!("library.colAlbum").to_uppercase())
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate(),
            );
    }
    if width == RowWidth::Wide {
        r = r.child(
            el().key("tracks-header-format")
                .w(52.0)
                .child(label(t!("library.colFormat").to_uppercase())),
        );
    }
    r = r.child(label(t!("library.colDuration").to_uppercase()).w(56.0));
    if show_loudness {
        r = r.child(label(t!("library.colDr").to_uppercase()).w(44.0));
        if width == RowWidth::Wide {
            r = r.child(label(t!("library.colLufs").to_uppercase()).w(64.0));
        }
    }
    // A blank spacer the same size as the row's `+` enqueue button (icon +
    // its padding), so the header's trailing edge lines up with the rows'.
    r.child(
        el().key("tracks-header-enqueue-spacer")
            .padding(space::SNUG)
            .child(icon(&icons::PLUS, 14.0).color(p.text_secondary.alpha(0.0))),
    )
}

/// The Tracks search bar: a bounded, left-aligned field in the shared
/// [`page_search_bar`] top (keyed `tracks-search-bar`), matching the
/// Albums/Artists/Genres/Playlists search shape. The field itself shows the
/// current filter and, while active, a cursor; clicking it (or pressing `/`)
/// starts typing and the inline ✕ clears it. No focused text widget — this is
/// keyboard-driven: `main::handle_key` routes keystrokes into `State::search`.
fn search_bar(s: &State, p: &Palette) -> El {
    let (shown, dim) = if s.search.is_empty() && !s.searching {
        (t!("library.searchPlaceholder").to_string(), true)
    } else if s.searching {
        (format!("{}│", s.search), false)
    } else {
        (s.search.clone(), false)
    };
    // The search "box": a magnifier + the query/placeholder in a muted rounded
    // field, mirroring `search_box`'s shape so the Tracks search reads the same as
    // the other browse pages'. `page_search_bar` bounds it to `SEARCH_MAX_W`.
    let mut field = row()
        .key("tracks-search")
        .role("button")
        .label(t!("library.searchPlaceholder").to_string())
        .focusable(true)
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(icon(&icons::SEARCH, 16.0).color(p.text_secondary))
        .child(
            text(shown)
                .font(type_::ITEM)
                .color(if dim { p.text_secondary } else { p.fg })
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        )
        .on_press(|st: &mut State| st.searching = true);
    // An inline ✕ clears the filter, nested in the field (Rune hit-tests the
    // innermost target, so pressing it clears without entering search mode).
    if !s.search.is_empty() {
        field = field.child(
            el().key("tracks-search-clear")
                .role("button")
                .label(t!("queue.clear").to_string())
                .focusable(true)
                .padding(space::XS)
                .radius(radius::SM)
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
    page_search_bar("tracks", field, Some(sort_toggle(s, p)))
}

/// The "Sort" toggle in the Tracks top bar (mirrors the Albums "Filters" button):
/// reveals/hides the sort-pill bar, so the pills aren't shown by default. Lit while
/// open or while a client-side loudness sort is active (so it reads as "a non-default
/// sort is on" even when collapsed).
fn sort_toggle(s: &State, p: &Palette) -> El {
    let lit = s.tracks_sort_open || s.loudness_sort.is_some();
    row()
        .key("tracks-sort-toggle")
        .role("button")
        .label(t!("library.sort").to_string())
        .focusable(true)
        .center_y()
        .gap(space::XS)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if lit { p.accent.alpha(0.2) } else { p.muted })
        .hover(p.fg.alpha(fx::HOVER))
        .child(
            text(t!("library.sort").to_string())
                .font(type_::ITEM)
                .color(if lit { p.accent } else { p.fg }),
        )
        .on_press(|st: &mut State| st.tracks_sort_open = !st.tracks_sort_open)
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
    // Three width tiers (see `RowWidth`): below the old app's `sm` (640) the
    // artist/album columns fold into a subtitle; the loudness/DR columns only appear
    // on a genuinely wide pane (> 760) since a medium row is already full of
    // artist/album/duration and they would overflow. First frame has no prior layout,
    // so we fall back to `Standard` (no fold, no loudness).
    let width = cx
        .layout_size("library-page")
        .map(|[w, _]| {
            if w <= 640.0 {
                RowWidth::Compact
            } else if w > 760.0 {
                RowWidth::Wide
            } else {
                RowWidth::Standard
            }
        })
        .unwrap_or(RowWidth::Standard);
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
                    .role("button")
                    .label(t!("library.addSource").to_string())
                    .focusable(true)
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
        // `role("list")` announces the rows as a list to assistive tech; each row
        // is a focusable button Tab reaches and Space plays. (Arrow keys stay the
        // app's global volume/seek shortcuts; per-list arrow roving is a follow-up.)
        let mut list = col().key("tracks").role("list").gap(space::XXS);
        for (i, t) in visible.iter().enumerate() {
            list = list.child(track_row(s, p, t, &ids, i, width, scale));
        }
        scrollbar::attach(
            list.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .grow(1.0)
            .min_h(0.0),
            "tracks",
            &LIB_SCROLL_THUMB.with(|t| t.clone()),
            "library-scrollbar",
            p,
        )
    };
    let mut page = col()
        .key("library-page")
        .grow(1.0)
        .min_h(0.0)
        .child(search_bar(s, p));
    // The sort-pill bar is revealed by the top bar's "Sort" toggle, so the Tracks
    // page opens clean like the other browse pages rather than showing a row of
    // pills by default.
    if s.tracks_sort_open {
        page = page.child(sort_bar(s, p));
    }
    page.child(track_header_row(p, width, s.show_loudness_columns))
        .child(body)
}

/// Shared sort-bar pill styling: filled with the primary color when active, muted
/// otherwise. `on_press` carries the specific choice (a baked axis or a loudness sort).
fn sort_pill(
    label: String,
    key: &str,
    active: bool,
    p: &Palette,
    on_press: impl Fn(&mut State) + 'static,
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
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
        .on_press(move |s: &mut State| on_press(s))
}

/// One baked-axis sort choice; the active axis is filled with the primary color.
fn sort_button(label: String, key: &str, axis: BrowseAxis, active: bool, p: &Palette) -> El {
    sort_pill(label, key, active, p, move |s: &mut State| {
        crate::set_sort(s, axis)
    })
}

/// One client-side loudness sort choice (DR / Loudness), driving `set_loudness_sort`
/// rather than a baked axis.
fn loudness_sort_button(
    label: String,
    key: &str,
    lkey: crate::LoudnessKey,
    active: bool,
    p: &Palette,
) -> El {
    sort_pill(label, key, active, p, move |s: &mut State| {
        crate::set_loudness_sort(s, lkey)
    })
}

/// The sort bar above the track list: switch which baked axis orders the library, or pick a
/// client-side loudness sort (DR / Loudness). When a loudness sort is active the axis pills
/// dim (the two are mutually exclusive). Wraps on a narrow pane so the extra pills stay
/// reachable rather than overflowing.
fn sort_bar(s: &State, p: &Palette) -> El {
    let a = s.sort_axis;
    let ls = s.loudness_sort;
    // An axis pill reads active only when no loudness sort is overriding it.
    let axis_active = |ax: BrowseAxis| ls.is_none() && a == ax;
    let mut bar = row()
        .key("sort-bar")
        .gap(space::SM)
        .padding(space::SM)
        .center_y()
        .flex_wrap()
        .child(text(t!("library.sort").to_string()).color(p.text_secondary))
        .child(sort_button(
            t!("library.sortArtist").to_string(),
            "sort-artist",
            BrowseAxis::ArtistAlbumTrack,
            axis_active(BrowseAxis::ArtistAlbumTrack),
            p,
        ))
        .child(sort_button(
            t!("library.sortAlbum").to_string(),
            "sort-album",
            BrowseAxis::Album,
            axis_active(BrowseAxis::Album),
            p,
        ))
        .child(sort_button(
            t!("library.sortTitle").to_string(),
            "sort-title",
            BrowseAxis::Title,
            axis_active(BrowseAxis::Title),
            p,
        ))
        .child(sort_button(
            t!("library.sortFolder").to_string(),
            "sort-folder",
            BrowseAxis::Folder,
            axis_active(BrowseAxis::Folder),
            p,
        ));
    // The DR / Loudness sort pills ride the loudness columns: they are hidden together
    // with them (the Appearance preference), so the list never offers a sort by a column
    // the user can't see. `set_show_loudness_columns` clears an active loudness sort when
    // the columns go away, so the base-axis pills read correctly here too.
    if s.show_loudness_columns {
        bar = bar
            .child(loudness_sort_button(
                t!("library.sortDr").to_string(),
                "sort-dr",
                crate::LoudnessKey::Dr,
                ls == Some(crate::LoudnessKey::Dr),
                p,
            ))
            .child(loudness_sort_button(
                t!("library.sortLoudness").to_string(),
                "sort-loudness",
                crate::LoudnessKey::Loudness,
                ls == Some(crate::LoudnessKey::Loudness),
                p,
            ));
    }
    bar
}

#[cfg(test)]
mod tests {
    use rune::{Env, Runtime};

    use super::*;

    fn tv(id: u32, format: Option<&str>) -> TrackView {
        TrackView {
            id,
            title: Some(format!("T{id}")),
            artist: Some("A".into()),
            album: Some("Alb".into()),
            genre: Some("Rock".into()),
            duration_secs: 1.0,
            remote_source: None,
            original_date: 0,
            release_date: 0,
            loudness: None,
            format: format.map(str::to_string),
        }
    }

    /// Lossless, lossy and DSD each get their own color, and the lossy bucket is
    /// the neutral default everything else (unknown extensions included) falls
    /// back to.
    #[test]
    fn format_family_color_groups_lossless_dsd_and_a_neutral_lossy_default() {
        let p = crate::theme::palette(false);
        assert_eq!(format_family_color("FLAC", &p), p.format_lossless);
        assert_eq!(format_family_color("WAV", &p), p.format_lossless);
        assert_eq!(format_family_color("AIFF", &p), p.format_lossless);
        assert_eq!(format_family_color("ALAC", &p), p.format_lossless);
        assert_eq!(format_family_color("WV", &p), p.format_lossless);
        assert_eq!(format_family_color("DSF", &p), p.format_dsd);
        assert_eq!(format_family_color("DFF", &p), p.format_dsd);
        assert_eq!(format_family_color("MP3", &p), p.text_secondary);
        assert_eq!(format_family_color("AAC", &p), p.text_secondary);
        assert_eq!(format_family_color("OGG", &p), p.text_secondary);
        assert_eq!(format_family_color("OPUS", &p), p.text_secondary);
        // M4A can't say AAC vs ALAC from the container alone, so it stays
        // grouped with the neutral default, matching the old app.
        assert_eq!(format_family_color("M4A", &p), p.text_secondary);
    }

    /// The Tracks search predicate: a case-insensitive substring over title /
    /// artist / album (the caller lowercases the query). The whole Tracks search
    /// path was previously untested — unlike the Artists/Genres in-page searches.
    #[test]
    fn track_matches_searches_title_artist_and_album_but_not_genre() {
        let t = TrackView {
            title: Some("Blue in Green".into()),
            artist: Some("Miles Davis".into()),
            album: Some("Kind of Blue".into()),
            ..tv(1, None) // genre = Some("Rock")
        };

        // Each searchable field matches as a SUBSTRING (not a prefix/exact), and the
        // field side is lowercased so mixed-case metadata matches a lowercase query.
        assert!(track_matches(&t, "green"), "title substring");
        assert!(track_matches(&t, "miles"), "artist substring");
        assert!(track_matches(&t, "kind of"), "album substring");
        // An empty query matches everything (the "no filter" case).
        assert!(track_matches(&t, ""));
        // A term in none of the three searched fields does not match …
        assert!(!track_matches(&t, "zebra"));
        // … and GENRE is deliberately not searched (the row carries Some("Rock")).
        assert!(
            !track_matches(&t, "rock"),
            "genre is not one of the searched fields"
        );

        // All-None metadata never matches and never panics.
        let empty = TrackView {
            title: None,
            artist: None,
            album: None,
            ..tv(2, None)
        };
        assert!(!track_matches(&empty, "anything"));
    }

    /// End to end through the real Tracks table: a seeded library with a
    /// lossless, a lossy, a DSD and an unknown-format track shows a FORMAT
    /// badge for every track that carries one, with the label text present,
    /// and no badge at all for the track with no known format.
    #[test]
    fn track_list_renders_a_format_badge_for_every_track_with_a_known_format() {
        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            track_list(s, &p, cx, scale)
        }

        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());
        s.library = vec![
            tv(1, Some("FLAC")),
            tv(2, Some("MP3")),
            tv(3, Some("DSF")),
            tv(4, None),
        ];

        let env = Env {
            system_dark: true,
            scale: 2.0,
        };
        let mut rt = Runtime::new(s, test_view, (1400.0, 720.0), env);
        rt.frame(0.0);

        assert!(
            rt.find("format-badge-1").is_some(),
            "a lossless track shows its FORMAT badge"
        );
        assert!(
            rt.find("format-badge-2").is_some(),
            "a lossy track shows its FORMAT badge"
        );
        assert!(
            rt.find("format-badge-3").is_some(),
            "a DSD track shows its FORMAT badge"
        );
        assert!(
            rt.find("format-badge-4").is_none(),
            "a track with no known format shows no badge"
        );

        let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
        assert!(texts.iter().any(|t| t == "FLAC"), "FLAC label renders");
        assert!(texts.iter().any(|t| t == "MP3"), "MP3 label renders");
        assert!(texts.iter().any(|t| t == "DSF"), "DSF label renders");
    }

    /// The per-track loudness columns (DR + LUFS cells) and their DR/Loudness sort
    /// pills are behind the Appearance `show_loudness_columns` preference: all present
    /// when on, all gone when off. Wide pane so LUFS is width-eligible, isolating the
    /// preference as the only gate under test.
    #[test]
    fn loudness_columns_follow_the_appearance_preference() {
        let _locale = crate::config::use_test_locale("en");
        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            col()
                .child(sort_bar(s, &p))
                .child(track_list(s, &p, cx, scale))
        }
        let present = |show: bool| {
            let env = Env {
                system_dark: true,
                scale: 2.0,
            };
            let mut s = State::new(soul_player_host::PlayerHandle::offline());
            s.show_loudness_columns = show;
            s.library = vec![tv(1, Some("FLAC"))];
            let mut rt = Runtime::new(s, test_view, (1400.0, 720.0), env);
            rt.frame(0.0);
            (
                rt.find("dr-1").is_some(),
                rt.find("lufs-1").is_some(),
                rt.find("sort-dr").is_some(),
                rt.find("sort-loudness").is_some(),
            )
        };
        assert_eq!(
            present(true),
            (true, true, true, true),
            "DR + LUFS cells and both loudness sort pills show when the preference is on"
        );
        assert_eq!(
            present(false),
            (false, false, false, false),
            "they all disappear when the preference is off"
        );
    }

    /// End to end through the real Tracks table: the column-header row renders
    /// above the list, and its labels match `track_row`'s own tier gating:
    /// TITLE/DURATION/DR always show; ARTIST/ALBUM join at Standard+; FORMAT
    /// and LUFS only join at Wide.
    #[test]
    fn tracks_header_row_shows_the_labels_its_tier_reveals() {
        let _locale = crate::config::use_test_locale("en");
        fn test_view(s: &State, cx: &mut Cx<State>) -> El {
            let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
            let scale = cx.env().scale;
            track_list(s, &p, cx, scale)
        }

        fn render(width: f32) -> Runtime<State> {
            let mut s = State::new(soul_player_host::PlayerHandle::offline());
            s.library = vec![tv(1, Some("FLAC"))];
            let env = Env {
                system_dark: true,
                scale: 2.0,
            };
            let mut rt = Runtime::new(s, test_view, (width, 720.0), env);
            rt.frame(0.0);
            rt
        }

        let texts = |rt: &Runtime<State>| -> Vec<String> {
            rt.elements().into_iter().filter_map(|e| e.text).collect()
        };

        // Wide (> 760 content pane): every column's header shows.
        let wide = render(1400.0);
        assert!(
            wide.find("tracks-header").is_some(),
            "the header row renders"
        );
        let wide_texts = texts(&wide);
        assert!(wide_texts.iter().any(|t| t == "TITLE"));
        assert!(wide_texts.iter().any(|t| t == "ARTIST"));
        assert!(wide_texts.iter().any(|t| t == "ALBUM"));
        assert!(wide_texts.iter().any(|t| t == "FORMAT"));
        assert!(wide_texts.iter().any(|t| t == "DURATION"));
        assert!(wide_texts.iter().any(|t| t == "DR"));
        assert!(wide_texts.iter().any(|t| t == "LUFS"));

        // Standard (a mid content pane, 641..=760): artist/album join, but not
        // the wide-only FORMAT/LUFS columns.
        let standard = render(700.0);
        let standard_texts = texts(&standard);
        assert!(standard_texts.iter().any(|t| t == "TITLE"));
        assert!(standard_texts.iter().any(|t| t == "ARTIST"));
        assert!(standard_texts.iter().any(|t| t == "ALBUM"));
        assert!(standard_texts.iter().any(|t| t == "DURATION"));
        assert!(!standard_texts.iter().any(|t| t == "FORMAT"));
        assert!(!standard_texts.iter().any(|t| t == "LUFS"));

        // Compact (a narrow content pane): artist/album fold away too.
        let compact = render(500.0);
        let compact_texts = texts(&compact);
        assert!(compact_texts.iter().any(|t| t == "TITLE"));
        assert!(compact_texts.iter().any(|t| t == "DURATION"));
        assert!(!compact_texts.iter().any(|t| t == "ARTIST"));
        assert!(!compact_texts.iter().any(|t| t == "ALBUM"));
    }
}
