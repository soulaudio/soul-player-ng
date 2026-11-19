//! The "Add to playlist" overlay: opened from a track row's context menu, it
//! is a MEMBERSHIP MANAGER over the user's playlists, not a one-shot picker.
//! Each row shows whether the pending track is already in that playlist (a
//! checkbox) and pressing the row toggles membership immediately and
//! persists, without closing the overlay, so a track can be added to or
//! removed from several playlists in one visit. The inline "New playlist..."
//! field creates a playlist and adds the track to it, also without closing.
//! The overlay only closes via its explicit "Done" button, the dim
//! backdrop, or Escape.
//! A centered, dim-backdrop modal (mirrors `main.rs`'s crash-report dialog)
//! rather than an anchored popover: its trigger is a native context-menu item
//! that has already closed by the time this renders, so there is no trigger
//! element left to anchor under.
#![forbid(unsafe_code)]

use rune::{Color, Cursor, El, col, el, icon, row, text, text_input};
use soul_player_host::TrackSnapshot;

use crate::State;
use crate::playlists::{PlaylistEntry, PlaylistId, PlaylistSummary};
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;

/// The membership checkbox's size: filled and checked when the track is
/// already in the playlist, outline-only when not. Smaller than
/// `views::settings`'s own enable/disable toggle box (`TOGGLE_PX`) since
/// this sits inside a denser list row.
const MEMBER_BOX_PX: f32 = 18.0;
/// The check glyph's size inside [`MEMBER_BOX_PX`].
const MEMBER_CHECK_PX: f32 = 12.0;

/// Build the `PlaylistEntry` for `track_id` from its durable locator (via
/// `host.locator_of`) and the on-screen `TrackView` for that id in
/// `s.library` (title/artist/album/duration; track/disc number aren't
/// captured yet), then append it to playlist `playlist_id`. This is the one
/// call site every "add to playlist" action shares (the overlay's pick list,
/// its New-playlist field, and any future surface), so the snapshot shape
/// never drifts between them.
///
/// A no-op if `track_id` doesn't resolve to a locator (no library open, or
/// the id has since vanished) or isn't found in `s.library`: there is
/// nothing durable to store.
pub(crate) fn add_track_to_playlist(s: &mut State, track_id: u32, playlist_id: PlaylistId) {
    let Some(locator) = s.host.locator_of(track_id) else {
        return;
    };
    let Some(view) = s.library.iter().find(|t| t.id == track_id) else {
        return;
    };
    let snapshot = TrackSnapshot {
        title: view.title.clone().unwrap_or_default(),
        artist: view.artist.clone().unwrap_or_default(),
        album: view.album.clone().unwrap_or_default(),
        duration_secs: view.duration_secs.round() as u32,
        track_no: None,
        disc_no: None,
    };
    s.playlists.add_entry(
        playlist_id,
        PlaylistEntry {
            locator,
            snapshot,
            mbid: None,
        },
    );
}

/// Whether playlist `playlist_id` already holds `track_id`, compared by
/// durable-locator key (so each row's checkbox reflects live membership
/// instead of inviting a blind duplicate). `false` if the track's locator
/// doesn't resolve: nothing to compare against.
fn already_has(s: &State, playlist_id: PlaylistId, track_id: u32) -> bool {
    let Some(locator) = s.host.locator_of(track_id) else {
        return false;
    };
    let key = locator.key();
    s.playlists
        .get(playlist_id)
        .is_some_and(|pl| pl.entries.iter().any(|e| e.locator.key() == key))
}

/// Remove `track_id` from playlist `playlist_id`, the inverse of
/// [`add_track_to_playlist`]: resolve the track's durable locator, find the
/// entry in `playlist_id` whose own locator shares its key, and remove that
/// entry by index. A no-op (never panics, never removes the wrong entry) if
/// the locator doesn't resolve, the playlist isn't loaded, or no entry
/// matches: the track simply isn't in this playlist.
pub(crate) fn remove_track_from_playlist(s: &mut State, track_id: u32, playlist_id: PlaylistId) {
    let Some(locator) = s.host.locator_of(track_id) else {
        return;
    };
    let key = locator.key();
    let Some(playlist) = s.playlists.get(playlist_id) else {
        return;
    };
    let Some(index) = playlist.entries.iter().position(|e| e.locator.key() == key) else {
        return;
    };
    s.playlists.remove_entry(playlist_id, index);
}

/// Toggle `track_id`'s membership in playlist `playlist_id`: remove it if
/// already present, add it otherwise. The one action every row/button in
/// this overlay drives now that it's a membership manager rather than a
/// one-shot picker.
pub(crate) fn toggle_track_in_playlist(s: &mut State, track_id: u32, playlist_id: PlaylistId) {
    if already_has(s, playlist_id, track_id) {
        remove_track_from_playlist(s, track_id, playlist_id);
    } else {
        add_track_to_playlist(s, track_id, playlist_id);
    }
}

/// The membership checkbox: filled with a check glyph when `checked` (the
/// track IS in this playlist), outline-only (muted fill, bordered, no
/// glyph) when not. Mirrors `views::settings`'s own enable/disable toggle
/// box shape, scaled down for an inline list row.
fn membership_box(checked: bool, p: &Palette) -> El {
    let mut b = el()
        .size(MEMBER_BOX_PX, MEMBER_BOX_PX)
        .radius(radius::XS)
        .center()
        .fill(if checked { p.primary } else { p.muted });
    if !checked {
        b = b.border(1.0, p.border);
    }
    if checked {
        b = b.child(icon(&icons::CHECK, MEMBER_CHECK_PX).color(p.bg));
    }
    b
}

/// One playlist row in the membership list: a checkbox reflecting whether
/// `track_id` is already in this playlist, its name, and its track count.
/// Pressing the row toggles membership (via [`toggle_track_in_playlist`])
/// without closing the overlay, so a track can be ticked into (or out of)
/// several playlists in one visit; the overlay only closes via the "Done"
/// button, the backdrop, or Escape.
fn pick_row(s: &State, p: &Palette, track_id: u32, summary: &PlaylistSummary) -> El {
    let id = summary.id;
    let checked = already_has(s, id, track_id);
    let count = text(t!("playlists.trackCount", count => summary.count).to_string())
        .font(type_::CAPTION)
        .color(p.text_secondary);
    row()
        .key(format!("add-to-playlist-pick-{id}"))
        .role("button")
        .label(summary.name.clone())
        .checked(checked)
        .focusable(true)
        .w_full()
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(membership_box(checked, p))
        .child(
            text(summary.name.clone())
                .font(type_::ITEM)
                .color(p.fg)
                .grow(1.0)
                .truncate(),
        )
        .child(count)
        .on_press(move |st: &mut State| {
            toggle_track_in_playlist(st, track_id, id);
        })
}

/// Commit the overlay's inline New-playlist field: a blank/whitespace-only
/// name is a no-op (mirrors `views::playlists::confirm_new_playlist`);
/// otherwise create the playlist, add the pending track to it, reset the
/// field, and KEEP the overlay open (the fresh playlist's row then renders
/// checked). A no-op if the overlay isn't actually open.
fn confirm_new_playlist(s: &mut State) {
    let Some(track_id) = s.add_to_playlist_for else {
        return;
    };
    let name = s.add_to_playlist_buf.text.trim().to_string();
    if name.is_empty() {
        return;
    }
    let id = s.playlists.create(&name);
    add_track_to_playlist(s, track_id, id);
    s.add_to_playlist_buf = rune::TextEdit::new("");
}

/// The inline "New playlist..." affordance: a text field bound to
/// `State::add_to_playlist_buf`, plus a confirm button, either of which
/// commits (Enter in the field, or pressing the button). Mirrors
/// `views::playlists::new_playlist_row`.
fn new_playlist_field(s: &State, p: &Palette) -> El {
    row()
        .key("add-to-playlist-new-group")
        .w_full()
        .gap(space::SM)
        .child(
            text_input(&s.add_to_playlist_buf, type_::ITEM)
                .key("add-to-playlist-new-input")
                .label(t!("playlists.addToNew").to_string())
                .placeholder(t!("playlists.addToNew").to_string(), p.text_secondary)
                .single_line()
                .grow(1.0)
                .px(space::SM)
                .py(space::XS)
                .radius(radius::SM)
                .fill(p.muted)
                .color(p.fg)
                .on_edit(|st: &mut State| &mut st.add_to_playlist_buf)
                .on_submit(|st: &mut State, _tx| confirm_new_playlist(st)),
        )
        .child(
            el().key("add-to-playlist-new-confirm")
                .role("button")
                .label(t!("playlists.addToNew").to_string())
                .focusable(true)
                .px(space::MD)
                .py(space::XS)
                .radius(radius::SM)
                .fill(p.primary)
                .cursor(Cursor::Pointer)
                .child(text(t!("playlists.addToNew").to_string()).color(p.bg))
                .on_press(confirm_new_playlist),
        )
}

/// The explicit close affordance: a right-aligned "Done" button, the
/// membership manager's only in-dialog way to close (besides the dim
/// backdrop press / Escape that `El::modal` already wires). Rows no longer
/// close the overlay themselves, so without this there would be no way to
/// leave once satisfied short of dismissing via the backdrop.
fn done_row(p: &Palette) -> El {
    row()
        .key("add-to-playlist-done-row")
        .w_full()
        .justify_end()
        .child(
            el().key("add-to-playlist-done")
                .role("button")
                .label(t!("playlists.done").to_string())
                .focusable(true)
                .px(space::MD)
                .py(space::XS)
                .radius(radius::SM)
                .fill(p.primary)
                .cursor(Cursor::Pointer)
                .child(text(t!("playlists.done").to_string()).color(p.bg))
                .on_press(|st: &mut State| crate::close_add_to_playlist(st)),
        )
}

/// The "Add to playlist" overlay, mounted at the window root (via
/// [`crate::view`]'s overlay layer): a centered, dim-backdrop modal listing
/// every playlist (or the empty-state message) over the inline New-playlist
/// field, with an explicit "Done" button to close it. `None` while
/// `State::add_to_playlist_for` is `None` (closed).
pub fn overlay(s: &State, p: &Palette) -> Option<El> {
    let track_id = s.add_to_playlist_for?;
    let summaries = s.playlists.list();
    let mut list = col()
        .key("add-to-playlist-list")
        .role("list")
        .gap(space::XXS);
    if summaries.is_empty() {
        list = list.child(
            text(t!("playlists.empty").to_string())
                .font(type_::ITEM)
                .color(p.text_secondary),
        );
    } else {
        for summary in &summaries {
            list = list.child(pick_row(s, p, track_id, summary));
        }
    }
    Some(
        col()
            .key("add-to-playlist-modal")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .fill(Color::BLACK.alpha(0.45))
            .child(
                col()
                    .key("add-to-playlist-dialog")
                    .w(360.0)
                    .fill(p.card)
                    .radius(radius::LG)
                    .border(1.0, p.border)
                    .padding(space::LG)
                    .gap(space::MD)
                    .modal(true, crate::close_add_to_playlist)
                    .child(
                        text(t!("playlists.addTo").to_string())
                            .font(type_::TITLE)
                            .color(p.fg),
                    )
                    .child(list)
                    .child(new_playlist_field(s, p))
                    .child(done_row(p)),
            ),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use rune::{Cx, Env, Ime, Key, KeyEvent, Mods, Runtime};
    use soul_player_host::{PlayerHandle, TrackLocator};

    use super::*;
    use crate::config;
    use crate::playlists::PlaylistStore;

    fn env() -> Env {
        Env {
            system_dark: true,
            scale: 2.0,
        }
    }

    /// Render just the overlay (the same "skip the app shell" pattern other
    /// view tests use), falling back to an empty placeholder when it's
    /// closed so the view fn always returns an `El`.
    fn test_view(s: &State, cx: &mut Cx<State>) -> El {
        let p = crate::theme::palette(crate::is_dark(s.theme_pref, cx.system_dark()));
        overlay(s, &p).unwrap_or_else(|| el().key("add-to-playlist-none"))
    }

    /// A minimal valid stereo PCM WAV (silence), just enough for the importer
    /// to scan it into one cached track. Mirrors `config.rs`'s own `tiny_wav`.
    fn tiny_wav() -> Vec<u8> {
        let (rate, frames, channels, bits): (u32, u32, u16, u16) = (48_000, 4_800, 2, 16);
        let block_align = channels * bits / 8;
        let data_len = frames * u32::from(block_align);
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        v
    }

    /// A `Runtime<State>` backed by a real scanned temp library holding one
    /// file per `names`, so each track's durable locator resolves against a
    /// real `PlayerHandle` (as the "stores the real locator" test needs).
    /// Each call gets its own temp dir (a process-wide atomic counter
    /// alongside the process id), so two tests in this binary never collide.
    /// Caller must hold `config::use_test_config_dir()` for the duration.
    fn runtime_over_library(names: &[&str]) -> Runtime<State> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "soul_rune_add_to_playlist_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in names {
            std::fs::write(dir.join(name), tiny_wav()).unwrap();
        }
        let host = PlayerHandle::recording();
        host.open_folder(&dir).expect("scan the temp library");
        let s = State::new(host);
        let mut rt = Runtime::new(s, test_view, (1100.0, 720.0), env());
        rt.frame(0.0);
        rt
    }

    #[test]
    fn toggling_adds_then_removes_and_persists() {
        let _g = config::use_test_config_dir();
        let mut rt = runtime_over_library(&["a.wav"]);
        let pid = rt.state.playlists.create("P");
        let tid = rt.state.library[0].id;

        toggle_track_in_playlist(&mut rt.state, tid, pid);
        assert_eq!(
            PlaylistStore::load().get(pid).unwrap().entries.len(),
            1,
            "the first toggle adds the track"
        );

        toggle_track_in_playlist(&mut rt.state, tid, pid);
        assert_eq!(
            PlaylistStore::load().get(pid).unwrap().entries.len(),
            0,
            "the second toggle removes it again"
        );
    }

    #[test]
    fn remove_is_a_no_op_when_absent() {
        let _g = config::use_test_config_dir();
        let mut rt = runtime_over_library(&["a.wav"]);
        let pid = rt.state.playlists.create("P");
        let tid = rt.state.library[0].id;

        remove_track_from_playlist(&mut rt.state, tid, pid); // never added
        assert_eq!(PlaylistStore::load().get(pid).unwrap().entries.len(), 0);
    }

    #[test]
    fn add_to_playlist_stores_locator_and_snapshot() {
        let _g = config::use_test_config_dir();
        let mut rt = runtime_over_library(&["a.wav"]);
        let pid = rt.state.playlists.create("Target");
        let tid = rt.state.library[0].id;

        add_track_to_playlist(&mut rt.state, tid, pid);

        let loaded = PlaylistStore::load();
        let entry = &loaded.get(pid).unwrap().entries[0];
        assert!(
            matches!(&entry.locator, TrackLocator::Local { path } if path.ends_with("a.wav")),
            "the stored locator is the track's local path: {:?}",
            entry.locator,
        );
        assert_eq!(
            entry.snapshot.title,
            rt.state.library[0].title.clone().unwrap()
        );
    }

    /// Open the overlay for `track_id`, the same state change the library
    /// context menu's "Add to playlist" item drives. Bypasses the native
    /// popup itself (this harness, like `library.rs`'s own untested context
    /// menu, can't drive a real OS right-click menu).
    fn open_add_overlay(rt: &mut Runtime<State>, track_id: u32) {
        rt.update(move |st: &mut State, _tx| crate::open_add_to_playlist(st, track_id));
        rt.settle_all();
        rt.frame(0.0);
    }

    /// Press playlist `pid`'s row in the already-open overlay, the same
    /// toggle a real click on its membership checkbox drives. Panics if the
    /// row isn't found (the overlay isn't open, or `pid` isn't in the list).
    fn toggle_row(rt: &mut Runtime<State>, pid: PlaylistId) {
        let (x, y) = rt
            .find(&format!("add-to-playlist-pick-{pid}"))
            .expect("the playlist's pick row renders");
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    /// Drive the overlay's inline New-playlist field through the real
    /// harness: click to focus it, type `name` via IME commit, then confirm
    /// with Return. Mirrors `views::playlists`' own `trigger_new_playlist`.
    fn overlay_new_playlist(rt: &mut Runtime<State>, name: &str) {
        let (x, y) = rt
            .find("add-to-playlist-new-input")
            .expect("the overlay's New-playlist field renders");
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        rt.ime(Ime::Commit(name.to_string()));
        rt.frame(0.0);
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Mods::default(),
        });
        rt.frame(0.0);
    }

    /// Confirming the New-playlist field creates the playlist and adds the
    /// pending track to it, same as before this task; what changed is that
    /// a membership manager keeps the overlay open afterward (Task 1 no
    /// longer treats confirming as a terminal "pick and close" action), so
    /// the newly-created playlist's own row can render checked.
    #[test]
    fn add_overlay_new_playlist_keeps_overlay_open_and_adds() {
        let _g = config::use_test_config_dir();
        let mut rt = runtime_over_library(&["a.wav"]);
        let tid = rt.state.library[0].id;

        open_add_overlay(&mut rt, tid);
        assert!(
            rt.find("add-to-playlist-dialog").is_some(),
            "the overlay renders once opened"
        );

        overlay_new_playlist(&mut rt, "Brand New");

        let store = PlaylistStore::load();
        let created = store
            .list()
            .iter()
            .find(|pl| pl.name == "Brand New")
            .expect("the new playlist exists on disk")
            .id;
        assert_eq!(store.get(created).unwrap().entries.len(), 1);
        assert_eq!(
            rt.state.add_to_playlist_for,
            Some(tid),
            "creating a playlist keeps the membership-manager overlay open"
        );
    }

    /// Pressing an existing playlist's row toggles the pending track's
    /// membership in it: the first press adds it (ticking the row) without
    /// closing the overlay, and a second press on the SAME row removes it
    /// again. Neither press touches the New-playlist field.
    #[test]
    fn add_overlay_pick_existing_playlist_toggles_membership() {
        let _g = config::use_test_config_dir();
        let mut rt = runtime_over_library(&["a.wav"]);
        let tid = rt.state.library[0].id;
        let pid = rt.state.playlists.create("Existing");

        open_add_overlay(&mut rt, tid);

        toggle_row(&mut rt, pid);
        assert_eq!(PlaylistStore::load().get(pid).unwrap().entries.len(), 1);
        assert_eq!(
            rt.state.add_to_playlist_for,
            Some(tid),
            "adding via the row keeps the overlay open"
        );

        toggle_row(&mut rt, pid);
        assert_eq!(
            PlaylistStore::load().get(pid).unwrap().entries.len(),
            0,
            "pressing the same row again removes it"
        );
    }

    #[test]
    fn overlay_stays_open_after_a_toggle() {
        let _g = config::use_test_config_dir();
        let mut rt = runtime_over_library(&["a.wav"]);
        let pid = rt.state.playlists.create("P");
        let tid = rt.state.library[0].id;

        open_add_overlay(&mut rt, tid);
        // Toggling a row must NOT close the overlay (membership manager,
        // not a picker).
        toggle_row(&mut rt, pid);

        assert_eq!(rt.state.add_to_playlist_for, Some(tid));
    }

    /// The explicit "Done" button is the membership manager's own way to
    /// close once the overlay's rows don't close it themselves anymore.
    #[test]
    fn done_button_closes_the_overlay() {
        let _g = config::use_test_config_dir();
        let mut rt = runtime_over_library(&["a.wav"]);
        let tid = rt.state.library[0].id;

        open_add_overlay(&mut rt, tid);
        let (x, y) = rt
            .find("add-to-playlist-done")
            .expect("the Done button renders");
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);

        assert_eq!(rt.state.add_to_playlist_for, None);
    }
}
