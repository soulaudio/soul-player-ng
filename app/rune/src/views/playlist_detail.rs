//! A playlist's detail page: the shared detail hero (cover/icon, name, "{n}
//! tracks · {duration}", Play All, rename, delete) over its track list.
//! Reuses `detail_hero`/`play_all_pill` (see `views::detail_page`) and the
//! album/genre track row `detail_track_row` for every entry whose durable
//! locator still resolves against the live library; an entry whose track has
//! moved, been deleted, or whose source went offline renders greyed from the
//! captured `PlaylistEntry::snapshot` instead, with no play affordance.
//! Mirrors `genres::genre_detail` for the detail-page shape.
#![forbid(unsafe_code)]

use rune::{Color, Cursor, El, Icon, ScrollSpec, col, el, icon, row, text, text_input};
use soul_player_host::TrackLocator;

use crate::State;
use crate::playlists::{Playlist, PlaylistEntry, PlaylistId};
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::albums::{detail_track_row, fmt_total_dur};
use crate::views::detail_page::{HeroArt, detail_hero, play_all_pill};
use crate::views::icons;

/// A row control's square hit box (remove/up/down/rename/delete icons).
/// Mirrors `views::settings::profile_icon_btn`'s 28 pt box.
const CTRL_BTN: f32 = 28.0;
/// The icon glyph drawn inside a [`CTRL_BTN`] box.
const CTRL_ICON: f32 = 16.0;

/// Seed the rename buffer from playlist `id`'s current name, focus it with
/// everything selected (typing replaces), and open the inline editor. Mirrors
/// `start_dsp_rename`.
fn start_playlist_rename(s: &mut State, id: PlaylistId) {
    let current = s
        .playlists
        .get(id)
        .map(|pl| pl.name.clone())
        .unwrap_or_default();
    s.playlist_rename_buf = rune::TextEdit::new(current);
    s.playlist_rename_buf.focused = true;
    s.playlist_rename_buf.anchor = 0; // select-all: caret at end, anchor at start
    s.playlist_renaming = Some(id);
}

/// Commit the open rename: a blank/whitespace-only buffer is a no-op (the
/// playlist keeps its current name) rather than clearing it. Closes the
/// editor either way.
fn commit_playlist_rename(s: &mut State) {
    let Some(id) = s.playlist_renaming.take() else {
        return;
    };
    let name = s.playlist_rename_buf.text.trim().to_string();
    if !name.is_empty() {
        s.playlists.rename(id, &name);
    }
    s.playlist_rename_buf = rune::TextEdit::new("");
}

/// Close the rename editor without writing anything.
fn cancel_playlist_rename(s: &mut State) {
    s.playlist_renaming = None;
    s.playlist_rename_buf = rune::TextEdit::new("");
}

/// Open the delete-confirm step for playlist `id` (the Rename/Delete icons
/// are replaced by "Delete this playlist?" plus Delete/Cancel): a single
/// stray click on the Delete icon never deletes anything by itself.
fn request_playlist_delete(s: &mut State, id: PlaylistId) {
    s.playlist_delete_confirm = Some(id);
}

/// The confirm step's Delete action: delete the playlist and return to the
/// Playlists list (there is nothing left here to show).
fn confirm_playlist_delete(s: &mut State) {
    let Some(id) = s.playlist_delete_confirm.take() else {
        return;
    };
    s.playlists.delete(id);
    crate::go_back(s);
}

/// The confirm step's Cancel action: close it, deleting nothing.
fn cancel_playlist_delete(s: &mut State) {
    s.playlist_delete_confirm = None;
}

/// The tint a control's icon/glyph draws in: full `text_secondary` when
/// enabled, dimmed when not (the up/down boundary case).
fn ctrl_tint(p: &Palette, enabled: bool) -> Color {
    if enabled {
        p.text_secondary
    } else {
        p.text_secondary.alpha(0.35)
    }
}

/// A small icon-or-glyph control button: a [`CTRL_BTN`] box around `content`,
/// dimmed and inert (no role/focus/press) when `enabled` is false. Generalized
/// over `content` so both `Icon` glyphs (rename/delete/remove, which have a
/// dedicated `Icon`) and text glyphs (the up/down arrows, which don't) share
/// one implementation. Mirrors `views::settings::profile_icon_btn`.
fn ctrl_btn(
    key: String,
    label: String,
    enabled: bool,
    p: &Palette,
    content: El,
    on: impl Fn(&mut State) + 'static,
) -> El {
    let btn = el()
        .key(key)
        .role("button")
        .label(label)
        .focusable(enabled)
        .size(CTRL_BTN, CTRL_BTN)
        .center()
        .radius(radius::SM)
        .child(content);
    if enabled {
        btn.cursor(Cursor::Pointer)
            .hover(p.fg.alpha(fx::HOVER))
            .press(p.fg.alpha(fx::PRESS))
            .on_press(move |st: &mut State| on(st))
    } else {
        btn
    }
}

/// An icon-glyph control (remove/rename/delete/confirm/cancel): shorthand
/// over [`ctrl_btn`] for the common case of an `Icon` content.
fn icon_ctrl(
    key: String,
    glyph: &'static Icon,
    label: String,
    enabled: bool,
    p: &Palette,
    on: impl Fn(&mut State) + 'static,
) -> El {
    let tint = ctrl_tint(p, enabled);
    ctrl_btn(
        key,
        label,
        enabled,
        p,
        icon(glyph, CTRL_ICON).color(tint),
        on,
    )
}

/// The hero's action row: Play All (omitted entirely when nothing in the
/// playlist is playable, so a Play All click can never fire an empty
/// `PlayContext`), then either the Rename/Delete icon pair, an open inline
/// rename field, or an open delete-confirm prompt, per
/// `State::playlist_renaming`/`playlist_delete_confirm` for this playlist.
fn hero_actions(s: &State, p: &Palette, id: PlaylistId, play_ids: Vec<u32>) -> El {
    let mut r = row().key("playlist-actions").gap(space::SM).center_y();
    if !play_ids.is_empty() {
        r = r.child(
            play_all_pill("playlist-play-all", play_ids, p)
                .label(t!("playlists.playAll").to_string()),
        );
    }

    if s.playlist_renaming == Some(id) {
        r = r
            .child(
                text_input(&s.playlist_rename_buf, type_::ITEM)
                    .key("playlist-rename-input")
                    .label(t!("playlists.rename").to_string())
                    .single_line()
                    .px(space::SM)
                    .py(space::XS)
                    .radius(radius::SM)
                    .fill(p.muted)
                    .color(p.fg)
                    .on_edit(|st: &mut State| &mut st.playlist_rename_buf)
                    .on_submit(|st: &mut State, _tx| commit_playlist_rename(st)),
            )
            .child(icon_ctrl(
                "playlist-rename-confirm".to_string(),
                &icons::CHECK,
                t!("settings.confirm").to_string(),
                true,
                p,
                commit_playlist_rename,
            ))
            .child(icon_ctrl(
                "playlist-rename-cancel".to_string(),
                &icons::X,
                t!("settings.cancel").to_string(),
                true,
                p,
                cancel_playlist_rename,
            ));
    } else if s.playlist_delete_confirm == Some(id) {
        r = r
            .child(
                text(t!("playlists.deleteConfirm").to_string())
                    .font(type_::ITEM)
                    .color(p.text_secondary),
            )
            .child(icon_ctrl(
                "playlist-delete-confirm".to_string(),
                &icons::TRASH,
                t!("playlists.delete").to_string(),
                true,
                p,
                confirm_playlist_delete,
            ))
            .child(icon_ctrl(
                "playlist-delete-cancel".to_string(),
                &icons::X,
                t!("settings.cancel").to_string(),
                true,
                p,
                cancel_playlist_delete,
            ));
    } else {
        r = r
            .child(icon_ctrl(
                "playlist-rename".to_string(),
                &icons::PENCIL,
                t!("playlists.rename").to_string(),
                true,
                p,
                move |st: &mut State| start_playlist_rename(st, id),
            ))
            .child(icon_ctrl(
                "playlist-delete".to_string(),
                &icons::TRASH,
                t!("playlists.delete").to_string(),
                true,
                p,
                move |st: &mut State| request_playlist_delete(st, id),
            ));
    }
    r
}

/// A playlist row whose locator no longer resolves: greyed out, built from
/// the entry's captured `snapshot` rather than a live `TrackView`, with no
/// play affordance (no role/focus/press: there's nothing to play). Keyed
/// the same as a resolving row, so a caller doesn't need to know which kind
/// a given index is.
fn unavailable_row(p: &Palette, i: usize, entry: &PlaylistEntry) -> El {
    let dim = p.text_secondary.alpha(0.5);
    row()
        .key(format!("playlist-row-{i}"))
        .grow(1.0)
        .min_w(0.0)
        .gap(space::MD)
        .px(space::MD)
        .py(space::SM)
        .center_y()
        .child(
            text(format!("{}", i + 1))
                .font(type_::ITEM)
                .color(dim)
                .w(28.0),
        )
        .child(
            col()
                .gap(space::XXS)
                .min_w(0.0)
                .grow(1.0)
                .child(
                    text(entry.snapshot.title.clone())
                        .font(type_::ITEM)
                        .color(dim)
                        .truncate(),
                )
                .child(
                    text(t!("playlists.unavailable").to_string())
                        .font(type_::CAPTION)
                        .color(dim),
                ),
        )
}

/// One playlist entry: the shared track row (reused, re-keyed to
/// `playlist-row-{i}`) when its locator still resolves, else [`unavailable_row`]
/// built from the captured snapshot. Either way, three sibling controls sit
/// beside it: move up/down (disabled at the respective end of the list) and
/// remove, which act on the entry's position regardless of availability,
/// since cleaning up a playlist shouldn't require the track to still exist.
#[allow(clippy::too_many_arguments)]
fn entry_row(
    s: &State,
    p: &Palette,
    id: PlaylistId,
    i: usize,
    last: usize,
    entry: &PlaylistEntry,
    resolved: Option<u32>,
    play_ids: &[u32],
    play_index: Option<usize>,
) -> El {
    let track_el = resolved
        .and_then(|tid| s.library.iter().find(|t| t.id == tid))
        .map(|t| {
            detail_track_row(
                s.now_playing_id,
                s.playing,
                p,
                play_ids,
                play_index.unwrap_or(0),
                t,
            )
            .key(format!("playlist-row-{i}"))
            .grow(1.0)
        })
        .unwrap_or_else(|| unavailable_row(p, i, entry));

    let up_enabled = i > 0;
    let down_enabled = i < last;
    row()
        .key(format!("playlist-entry-{i}"))
        .gap(space::XS)
        .center_y()
        .child(track_el)
        .child(ctrl_btn(
            format!("playlist-up-{i}"),
            t!("playlists.moveUp").to_string(),
            up_enabled,
            p,
            text("\u{2191}")
                .color(ctrl_tint(p, up_enabled))
                .font(type_::ITEM),
            move |st: &mut State| st.playlists.move_entry(id, i, i - 1),
        ))
        .child(ctrl_btn(
            format!("playlist-down-{i}"),
            t!("playlists.moveDown").to_string(),
            down_enabled,
            p,
            text("\u{2193}")
                .color(ctrl_tint(p, down_enabled))
                .font(type_::ITEM),
            move |st: &mut State| st.playlists.move_entry(id, i, i + 1),
        ))
        .child(icon_ctrl(
            format!("playlist-remove-{i}"),
            &icons::TRASH,
            t!("playlists.removeTrack").to_string(),
            true,
            p,
            move |st: &mut State| st.playlists.remove_entry(id, i),
        ))
}

/// A back link to the Playlists list: a `←` glyph, matching `genres.rs`'s
/// `back_button`.
fn back_button(p: &Palette) -> El {
    el().key("playlist-back")
        .role("button")
        .label(t!("nav.playlists").to_string())
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

/// The back-link bar above the detail content.
fn row_top(p: &Palette) -> El {
    row()
        .key("playlist-detail-top")
        .px(space::PANE)
        .pt(space::SM)
        .child(back_button(p))
}

/// The fallback body when no playlist is open, or the open id has vanished
/// (e.g. deleted from elsewhere): just the way back.
fn missing_playlist(p: &Palette) -> El {
    col()
        .key("playlist-detail")
        .grow(1.0)
        .min_h(0.0)
        .padding(space::PANE)
        .child(back_button(p))
}

/// A playlist's detail page: a back link, the shared hero (cover/icon,
/// "PLAYLIST" eyebrow, name, "{n} tracks · {duration}", Play All, rename,
/// delete), then its track list. The playlist is `State::open_playlist`,
/// looked up in `State::playlists`; its entries' durable locators are
/// resolved against the live library ONCE per render via
/// `host.resolve_locators`.
pub fn page(s: &State, p: &Palette, scale: f32) -> El {
    let Some(id) = s.open_playlist else {
        return missing_playlist(p);
    };
    let playlist: &Playlist = match s.playlists.get(id) {
        Some(pl) => pl,
        None => return missing_playlist(p),
    };

    let locators: Vec<TrackLocator> = playlist.entries.iter().map(|e| e.locator.clone()).collect();
    let resolved: Vec<Option<u32>> = s.host.resolve_locators(&locators);

    // The "available" context Play All and every playable row's click share:
    // unavailable entries are skipped entirely, both from the ids Play All
    // issues and from the context a row's own click-to-play resumes into.
    let mut play_ids: Vec<u32> = Vec::new();
    let mut play_index_of: Vec<Option<usize>> = Vec::with_capacity(resolved.len());
    for r in &resolved {
        match r {
            Some(tid) => {
                play_index_of.push(Some(play_ids.len()));
                play_ids.push(*tid);
            }
            None => play_index_of.push(None),
        }
    }

    let total: f64 = playlist
        .entries
        .iter()
        .zip(&resolved)
        .map(|(e, r)| {
            r.and_then(|tid| s.library.iter().find(|t| t.id == tid))
                .map(|t| t.duration_secs)
                .unwrap_or(f64::from(e.snapshot.duration_secs))
        })
        .sum();
    let meta = t!(
        "detail.tracksAndDuration",
        count => playlist.entries.len(),
        dur => fmt_total_dur(total)
    )
    .to_string();

    // A user-set custom cover always wins over the cover-track/icon fallback,
    // matching the Playlists list page's own precedence (`playlists::playlist_cover`).
    let art = match s.playlists.custom_cover_path(id) {
        Some(path) => HeroArt::Image(path),
        None => match play_ids.first() {
            Some(&cid) if s.host.art(cid).is_some() => HeroArt::Cover {
                id: cid,
                circular: false,
            },
            _ => HeroArt::Icon(&icons::MUSIC),
        },
    };

    let hero = detail_hero(
        s,
        p,
        scale,
        "playlist",
        art,
        t!("playlists.eyebrow").to_uppercase(),
        playlist.name.clone(),
        None,
        meta,
        hero_actions(s, p, id, play_ids.clone()),
    );

    let last = playlist.entries.len().saturating_sub(1);
    let mut list = col().key("playlist-tracks").role("list").gap(space::XXS);
    for (i, entry) in playlist.entries.iter().enumerate() {
        list = list.child(entry_row(
            s,
            p,
            id,
            i,
            last,
            entry,
            resolved[i],
            &play_ids,
            play_index_of[i],
        ));
    }

    col()
        .key("playlist-detail")
        .grow(1.0)
        .min_h(0.0)
        .child(row_top(p))
        .child(
            col()
                .key("playlist-detail-content")
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
    use std::path::PathBuf;

    use rune::{Cx, Env, Ime, Key, KeyEvent, Mods, Runtime};
    use soul_player_host::{Command, PlayerHandle, TrackSnapshot, TrackView};

    use super::*;
    use crate::playlists::PlaylistStore;
    use crate::{Route, config};

    fn testdata_dir() -> PathBuf {
        // app/rune -> repo-root/testdata
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
    }

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

    fn click(rt: &mut Runtime<State>, key: &str) {
        let (x, y) = rt
            .find(key)
            .unwrap_or_else(|| panic!("element {key} not found"));
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    /// Whether any rendered element's text contains `needle`.
    fn shows(rt: &Runtime<State>, needle: &str) -> bool {
        rt.elements()
            .into_iter()
            .any(|e| e.text.as_deref().is_some_and(|t| t.contains(needle)))
    }

    /// Whether the element keyed `key` carries a play affordance (the button
    /// role `detail_track_row`/row controls attach): false for a plain,
    /// non-interactive row like [`unavailable_row`].
    fn row_is_playable(rt: &Runtime<State>, key: &str) -> bool {
        rt.elements()
            .into_iter()
            .find(|e| e.key.as_deref() == Some(key))
            .is_some_and(|e| e.role == "button")
    }

    fn entry_from(v: &TrackView, locator: TrackLocator) -> PlaylistEntry {
        PlaylistEntry {
            locator,
            snapshot: TrackSnapshot {
                title: v.title.clone().unwrap_or_default(),
                artist: v.artist.clone().unwrap_or_default(),
                album: v.album.clone().unwrap_or_default(),
                duration_secs: v.duration_secs.round() as u32,
                track_no: None,
                disc_no: None,
            },
            mbid: None,
        }
    }

    /// A playlist over the first two tracks of the real scanned testdata
    /// library (so their durable locators resolve against a real
    /// `PlayerHandle`), already open as `Route::Playlist`. Created on disk
    /// BEFORE `State::new` so its own `PlaylistStore::load()` picks it up
    /// (the same trick `main.rs`'s own playlist nav test uses). Caller must
    /// hold `config::use_test_config_dir()` for the duration.
    fn runtime_playlist_over_library() -> (Runtime<State>, PlaylistId) {
        let host = PlayerHandle::recording();
        let views = host.open_folder(&testdata_dir()).expect("scan testdata/");
        let loc_a = host.locator_of(views[0].id).expect("a resolves");
        let loc_b = host.locator_of(views[1].id).expect("b resolves");
        let mut store = PlaylistStore::load();
        let id = store.create("Mix");
        store.add_entry(id, entry_from(&views[0], loc_a));
        store.add_entry(id, entry_from(&views[1], loc_b));

        let mut s = State::new(host);
        s.route = Route::Playlist;
        s.open_playlist = Some(id);
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env());
        rt.frame(0.0);
        (rt, id)
    }

    /// A playlist holding exactly one entry whose locator never resolves,
    /// titled `title` in its captured snapshot, backed by `host`. Caller
    /// must hold `config::use_test_config_dir()` for the duration.
    fn runtime_playlist_with_one_missing_entry_on(
        host: PlayerHandle,
        title: &str,
    ) -> (Runtime<State>, PlaylistId) {
        let mut store = PlaylistStore::load();
        let id = store.create("Haunted");
        store.add_entry(
            id,
            PlaylistEntry {
                locator: TrackLocator::Local {
                    path: "D:/nowhere/gone.flac".into(),
                },
                snapshot: TrackSnapshot {
                    title: title.to_string(),
                    artist: "Nobody".into(),
                    album: "Void".into(),
                    duration_secs: 180,
                    track_no: None,
                    disc_no: None,
                },
                mbid: None,
            },
        );

        let mut s = State::new(host);
        s.route = Route::Playlist;
        s.open_playlist = Some(id);
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env());
        rt.frame(0.0);
        (rt, id)
    }

    /// Same as [`runtime_playlist_with_one_missing_entry_on`], backed by a
    /// plain `offline()` handle (no library, nothing recorded) for tests
    /// that only care about rendering, not what gets sent.
    fn runtime_playlist_with_one_missing_entry(title: &str) -> (Runtime<State>, PlaylistId) {
        runtime_playlist_with_one_missing_entry_on(PlayerHandle::offline(), title)
    }

    /// A temp directory unique to this call (process id + atomic counter),
    /// so parallel tests in this binary never collide. Mirrors
    /// `playlists::store`'s own test helper of the same shape.
    fn temp_source_dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "soul_rune_playlist_detail_cover_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write a tiny but real PNG (just the 8-byte file signature; it never
    /// needs to decode, only to exist as a `.png` file under the size cap)
    /// to a fresh temp dir and return its path. Mirrors `playlists::store`'s
    /// own `write_temp_png` test helper.
    fn write_temp_png() -> PathBuf {
        const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let path = temp_source_dir().join("source.png");
        std::fs::write(&path, PNG_SIGNATURE).unwrap();
        path
    }

    /// Same as [`runtime_playlist_over_library`], but with a custom cover
    /// image applied before the page ever renders, so the hero must prefer
    /// [`HeroArt::Image`] over the cover-track it would otherwise pick.
    fn runtime_playlist_over_library_with_custom_cover() -> (Runtime<State>, PlaylistId) {
        let host = PlayerHandle::recording();
        let views = host.open_folder(&testdata_dir()).expect("scan testdata/");
        let loc_a = host.locator_of(views[0].id).expect("a resolves");
        let mut store = PlaylistStore::load();
        let id = store.create("Mix");
        store.add_entry(id, entry_from(&views[0], loc_a));
        assert!(
            store.set_custom_cover(id, &write_temp_png()),
            "the custom cover applies"
        );

        let mut s = State::new(host);
        s.route = Route::Playlist;
        s.open_playlist = Some(id);
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env());
        rt.frame(0.0);
        (rt, id)
    }

    /// A playlist with three entries: two whose locators resolve against the
    /// real scanned testdata library (first and third) and one in between
    /// (second) that never resolves. Lets a test assert that remove/move act
    /// on the full entry-list index (matching the rendered row index, which
    /// includes unavailable rows), not an availability-filtered index.
    fn runtime_playlist_mixed_availability() -> (Runtime<State>, PlaylistId) {
        let host = PlayerHandle::recording();
        let views = host.open_folder(&testdata_dir()).expect("scan testdata/");
        let loc_a = host.locator_of(views[0].id).expect("a resolves");
        let loc_c = host.locator_of(views[1].id).expect("c resolves");
        let mut store = PlaylistStore::load();
        let id = store.create("Mixed");
        store.add_entry(id, entry_from(&views[0], loc_a));
        store.add_entry(
            id,
            PlaylistEntry {
                locator: TrackLocator::Local {
                    path: "D:/nowhere/gone.flac".into(),
                },
                snapshot: TrackSnapshot {
                    title: "Ghost Song".into(),
                    artist: "Nobody".into(),
                    album: "Void".into(),
                    duration_secs: 180,
                    track_no: None,
                    disc_no: None,
                },
                mbid: None,
            },
        );
        store.add_entry(id, entry_from(&views[1], loc_c));

        let mut s = State::new(host);
        s.route = Route::Playlist;
        s.open_playlist = Some(id);
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env());
        rt.frame(0.0);
        (rt, id)
    }

    #[test]
    fn play_all_issues_playcontext_with_resolved_ids_in_order() {
        let _g = config::use_test_config_dir();
        let (mut rt, id) = runtime_playlist_over_library();
        let locators: Vec<TrackLocator> = rt
            .state
            .playlists
            .get(id)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.locator.clone())
            .collect();
        let expected_ids: Vec<u32> = rt
            .state
            .host
            .resolve_locators(&locators)
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(
            expected_ids.len(),
            2,
            "both entries resolve against the scanned library"
        );

        click(&mut rt, "playlist-play-all");
        let cmd = rt
            .state
            .host
            .recorded_commands()
            .last()
            .cloned()
            .expect("Play All sent a command");
        match cmd {
            Command::PlayContext { ids, start: 0 } => assert_eq!(
                ids, expected_ids,
                "Play All plays exactly the playlist's resolved ids, in entry order"
            ),
            other => panic!("expected PlayContext starting at 0, got {other:?}"),
        }
    }

    #[test]
    fn play_all_is_absent_and_sends_nothing_when_every_entry_is_unavailable() {
        let _g = config::use_test_config_dir();
        let (rt, _id) =
            runtime_playlist_with_one_missing_entry_on(PlayerHandle::recording(), "Ghost Song");
        assert!(
            rt.find("playlist-play-all").is_none(),
            "no Play All pill is rendered when nothing in the playlist is playable"
        );
        assert!(
            rt.state.host.recorded_commands().is_empty(),
            "no PlayContext (empty or otherwise) is ever sent for an all-unavailable playlist"
        );
    }

    #[test]
    fn remove_and_move_act_on_the_full_entry_list_index_not_an_availability_filtered_one() {
        let _g = config::use_test_config_dir();
        let (mut rt, id) = runtime_playlist_mixed_availability();
        let locators_before: Vec<TrackLocator> = rt
            .state
            .playlists
            .get(id)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.locator.clone())
            .collect();
        assert_eq!(locators_before.len(), 3, "A, unavailable B, C");

        // Row index 1 is the UNAVAILABLE middle entry (it still occupies a
        // rendered row, per `unavailable_row`), not the availability-filtered
        // "second playable track" (which would be C). Removing row 1 must
        // drop B, leaving [A, C].
        click(&mut rt, "playlist-remove-1");
        let after_remove: Vec<TrackLocator> = rt
            .state
            .playlists
            .get(id)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.locator.clone())
            .collect();
        assert_eq!(
            after_remove,
            vec![locators_before[0].clone(), locators_before[2].clone()],
            "removing entry-list index 1 drops B (the unavailable one), not C"
        );

        // The two remaining entries are both playable now; move_entry still
        // runs on the full (post-removal) entry-list index.
        click(&mut rt, "playlist-down-0");
        let after_move: Vec<TrackLocator> = rt
            .state
            .playlists
            .get(id)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.locator.clone())
            .collect();
        assert_eq!(
            after_move,
            vec![locators_before[2].clone(), locators_before[0].clone()],
            "moving entry-list index 0 down swaps the remaining pair to [C, A]"
        );
    }

    #[test]
    fn removing_a_row_updates_the_store_and_persists() {
        let _g = config::use_test_config_dir();
        let (mut rt, id) = runtime_playlist_over_library();
        click(&mut rt, "playlist-remove-0");
        assert_eq!(rt.state.playlists.get(id).unwrap().entries.len(), 1);
        assert_eq!(PlaylistStore::load().get(id).unwrap().entries.len(), 1);
    }

    #[test]
    fn an_unavailable_track_renders_greyed_from_snapshot_and_is_not_playable() {
        let _g = config::use_test_config_dir();
        let (rt, _id) = runtime_playlist_with_one_missing_entry("Ghost Song");
        assert!(shows(&rt, "Ghost Song"), "the snapshot title still renders");
        assert!(
            !row_is_playable(&rt, "playlist-row-0"),
            "an unavailable row has no play affordance"
        );
        assert!(!shows(&rt, "Nobody"), "unavailable rows show no artist");
    }

    #[test]
    fn deleting_requires_confirm_then_returns_to_the_list() {
        let _g = config::use_test_config_dir();
        let (mut rt, id) = runtime_playlist_over_library();
        click(&mut rt, "playlist-delete");
        assert!(
            PlaylistStore::load().get(id).is_some(),
            "a single click on Delete must not delete"
        );
        // The confirm row is a brand-new subtree (replacing the Rename/Delete
        // icons), so it enter-animates from opacity 0; settle instantly so it's
        // pickable (matches the settle_all pattern used elsewhere in this app).
        rt.settle_all();
        rt.frame(0.0);
        click(&mut rt, "playlist-delete-confirm");
        assert!(
            PlaylistStore::load().get(id).is_none(),
            "confirm deletes it"
        );
        assert!(
            matches!(rt.state.route, Route::Playlists),
            "and returns to the list"
        );
    }

    #[test]
    fn delete_cancel_keeps_the_playlist() {
        let _g = config::use_test_config_dir();
        let (mut rt, id) = runtime_playlist_over_library();
        click(&mut rt, "playlist-delete");
        rt.settle_all();
        rt.frame(0.0);
        click(&mut rt, "playlist-delete-cancel");
        assert_eq!(rt.state.playlist_delete_confirm, None);
        assert!(PlaylistStore::load().get(id).is_some());
    }

    #[test]
    fn rename_commits_a_trimmed_name_and_persists() {
        let _g = config::use_test_config_dir();
        let (mut rt, id) = runtime_playlist_over_library();
        click(&mut rt, "playlist-rename");
        rt.ime(Ime::Commit("  Road Trip  ".to_string()));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        assert_eq!(
            rt.state.playlist_renaming, None,
            "the editor closes on confirm"
        );
        assert_eq!(rt.state.playlists.get(id).unwrap().name, "Road Trip");
        assert_eq!(PlaylistStore::load().get(id).unwrap().name, "Road Trip");
    }

    #[test]
    fn rename_cancel_leaves_the_name_untouched() {
        let _g = config::use_test_config_dir();
        let (mut rt, id) = runtime_playlist_over_library();
        click(&mut rt, "playlist-rename");
        rt.settle_all();
        rt.frame(0.0);
        click(&mut rt, "playlist-rename-cancel");
        assert_eq!(rt.state.playlist_renaming, None);
        assert_eq!(rt.state.playlists.get(id).unwrap().name, "Mix");
    }

    /// A playlist with a custom cover set renders the hero as
    /// `HeroArt::Image` (keyed `playlist-hero-image`), not the cover-track
    /// hero it would otherwise pick for a playlist whose first track has art.
    #[test]
    fn custom_cover_renders_the_hero_image_element() {
        let _g = config::use_test_config_dir();
        let (rt, _id) = runtime_playlist_over_library_with_custom_cover();
        assert!(
            rt.find("playlist-hero-image").is_some(),
            "a custom cover renders the keyed hero image element"
        );
        assert!(
            rt.find("playlist-cover").is_none(),
            "the custom cover takes precedence over the cover-track hero"
        );
    }

    /// Without a custom cover, the hero falls back to the existing
    /// cover-track/icon logic: no `playlist-hero-image` element at all.
    #[test]
    fn no_custom_cover_falls_back_to_the_existing_hero() {
        let _g = config::use_test_config_dir();
        let (rt, _id) = runtime_playlist_over_library();
        assert!(
            rt.find("playlist-hero-image").is_none(),
            "no custom cover set means no hero image element renders"
        );
    }
}
