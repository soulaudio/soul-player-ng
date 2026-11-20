//! The Cmd/Ctrl-K command palette: a flat registry of named actions (go to a
//! page, transport, view, playlist) that can be run without the mouse, the
//! substring filter its search field drives, and the overlay itself: the
//! query field, the live-filtered + highlighted list, and Up/Down/Enter/click
//! to run the highlighted command. `main.rs`'s `open_command_palette` /
//! `close_command_palette` own the `State::command_palette_open` flag
//! [`overlay`] reads; `main.rs`'s overlay-root mounts [`overlay`] alongside
//! the other global overlays (`add_to_playlist`, `signal_path`, ...).
#![forbid(unsafe_code)]

use rune::{Color, Cursor, El, Key, ScrollSpec, col, row, text, text_input};

use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::{Command, RepeatMode, Route, SettingsTab, State, ThemePref};

/// One entry in the command registry: a stable id (the key for the
/// localized label in [`command_label`], and the overlay's future element
/// key) and the action it runs. A plain `fn` pointer rather than a closure,
/// so the registry is cheap to rebuild on every render (`commands` is
/// precondition-filtered against the live `State`) and never captures stale
/// state of its own.
pub(crate) struct PaletteCommand {
    pub id: &'static str,
    pub action: fn(&mut State),
}

// ---- Nav commands: "Go to X" ----

/// Mirror the sidebar nav item's own `on_press` reset
/// (`views::sidebar::nav_item`): clear the back-stack and the page-scoped
/// ephemeral fields it resets, then land on `route`. Every "Go to X" command
/// below shares this, so jumping via the palette behaves exactly like a real
/// sidebar click rather than leaving stale nav history or a stray
/// New-genre/New-playlist buffer behind.
fn go_to(s: &mut State, route: Route) {
    s.nav_history.clear();
    s.genre_search = rune::TextEdit::new("");
    s.playlist_new_buf = rune::TextEdit::new("");
    s.route = route;
    s.mobile_show_content = true;
}

fn nav_home(s: &mut State) {
    go_to(s, Route::Home);
}
fn nav_albums(s: &mut State) {
    go_to(s, Route::Albums);
}
fn nav_artists(s: &mut State) {
    go_to(s, Route::Artists);
}
fn nav_genres(s: &mut State) {
    go_to(s, Route::Genres);
}
fn nav_playlists(s: &mut State) {
    go_to(s, Route::Playlists);
}
fn nav_tracks(s: &mut State) {
    go_to(s, Route::Library);
}
/// "Go to Settings" also lands on the Appearance tab, Settings' own launch
/// default (its page isn't in the sidebar's `NAV_DEFS`, so there is no
/// existing reset to mirror besides the footer gear's plain route switch;
/// this additionally picks a deterministic tab rather than whatever a past
/// visit happened to leave selected).
fn nav_settings(s: &mut State) {
    go_to(s, Route::Settings);
    s.settings_tab = SettingsTab::Appearance;
}

// ---- Transport commands ----

fn transport_play_pause(s: &mut State) {
    s.host.send(Command::TogglePlay);
}
fn transport_next(s: &mut State) {
    s.host.send(Command::Next);
}
fn transport_prev(s: &mut State) {
    s.host.send(Command::Prev);
}
fn transport_stop(s: &mut State) {
    s.host.send(Command::Stop);
}
fn transport_shuffle(s: &mut State) {
    s.host.send(Command::SetShuffle(!s.shuffle));
}
/// Cycle Off -> All -> One -> Off, the same order and wraparound
/// `views::player::repeat_button` steps through on each press.
fn transport_repeat(s: &mut State) {
    let next = match s.repeat {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    };
    s.host.send(Command::SetRepeat(next));
}
fn transport_mute(s: &mut State) {
    s.host.send(Command::ToggleMute);
}
fn transport_clear_queue(s: &mut State) {
    s.host.send(Command::ClearQueue);
}

// ---- View commands: theme + zoom ----

fn view_theme_system(s: &mut State) {
    crate::set_theme_pref(s, ThemePref::System);
}
fn view_theme_light(s: &mut State) {
    crate::set_theme_pref(s, ThemePref::Light);
}
fn view_theme_dark(s: &mut State) {
    crate::set_theme_pref(s, ThemePref::Dark);
}
fn view_zoom_in(s: &mut State) {
    crate::set_zoom(s, crate::stepped_zoom(s.ui_zoom, 1));
}
fn view_zoom_out(s: &mut State) {
    crate::set_zoom(s, crate::stepped_zoom(s.ui_zoom, -1));
}
fn view_zoom_reset(s: &mut State) {
    crate::set_zoom(s, 1.0);
}

// ---- Playlist commands ----

/// Navigate to the Playlists page, where the real inline "New playlist"
/// field lives (`views::playlists::new_playlist_row`): a plain, honest
/// action for a palette command rather than silently minting an unnamed
/// playlist with no way to name it from here.
fn playlist_new(s: &mut State) {
    go_to(s, Route::Playlists);
}

/// Add the current track to a playlist: opens the membership-manager
/// overlay for `s.now_playing_id`, the same overlay the now-playing heart
/// opens. Only reachable via [`commands`]'s precondition filter while a
/// track is actually playing, so the `if let` here is defensive, not load
/// bearing.
fn playlist_add_current(s: &mut State) {
    if let Some(id) = s.now_playing_id {
        crate::open_add_to_playlist(s, id);
    }
}

/// The full command registry, precondition-filtered against the live
/// `State`: every entry is always offered except "Add current track to
/// playlist", which only appears while something is actually playing
/// (`s.now_playing_id.is_some()`) since there is nothing to add otherwise.
pub(crate) fn commands(s: &State) -> Vec<PaletteCommand> {
    let mut cmds = vec![
        PaletteCommand {
            id: "nav-home",
            action: nav_home,
        },
        PaletteCommand {
            id: "nav-albums",
            action: nav_albums,
        },
        PaletteCommand {
            id: "nav-artists",
            action: nav_artists,
        },
        PaletteCommand {
            id: "nav-genres",
            action: nav_genres,
        },
        PaletteCommand {
            id: "nav-playlists",
            action: nav_playlists,
        },
        PaletteCommand {
            id: "nav-tracks",
            action: nav_tracks,
        },
        PaletteCommand {
            id: "nav-settings",
            action: nav_settings,
        },
        PaletteCommand {
            id: "play-pause",
            action: transport_play_pause,
        },
        PaletteCommand {
            id: "next",
            action: transport_next,
        },
        PaletteCommand {
            id: "prev",
            action: transport_prev,
        },
        PaletteCommand {
            id: "stop",
            action: transport_stop,
        },
        PaletteCommand {
            id: "shuffle",
            action: transport_shuffle,
        },
        PaletteCommand {
            id: "repeat",
            action: transport_repeat,
        },
        PaletteCommand {
            id: "mute",
            action: transport_mute,
        },
        PaletteCommand {
            id: "clear-queue",
            action: transport_clear_queue,
        },
        PaletteCommand {
            id: "theme-system",
            action: view_theme_system,
        },
        PaletteCommand {
            id: "theme-light",
            action: view_theme_light,
        },
        PaletteCommand {
            id: "theme-dark",
            action: view_theme_dark,
        },
        PaletteCommand {
            id: "zoom-in",
            action: view_zoom_in,
        },
        PaletteCommand {
            id: "zoom-out",
            action: view_zoom_out,
        },
        PaletteCommand {
            id: "zoom-reset",
            action: view_zoom_reset,
        },
        PaletteCommand {
            id: "new-playlist",
            action: playlist_new,
        },
    ];
    if s.now_playing_id.is_some() {
        cmds.push(PaletteCommand {
            id: "add-to-playlist",
            action: playlist_add_current,
        });
    }
    cmds
}

/// The localized label for command `id`: a dynamic `t!` key isn't allowed,
/// so this maps each id to its literal `command.<id>` key, the same shape
/// `views::sidebar::nav_label` uses for nav ids. Falls back to the raw id
/// for anything unrecognized (visibly wrong rather than silently blank);
/// every id this module's own [`commands`] registry emits has a real entry
/// below.
pub(crate) fn command_label(id: &str) -> String {
    match id {
        "nav-home" => t!("command.nav-home").to_string(),
        "nav-albums" => t!("command.nav-albums").to_string(),
        "nav-artists" => t!("command.nav-artists").to_string(),
        "nav-genres" => t!("command.nav-genres").to_string(),
        "nav-playlists" => t!("command.nav-playlists").to_string(),
        "nav-tracks" => t!("command.nav-tracks").to_string(),
        "nav-settings" => t!("command.nav-settings").to_string(),
        "play-pause" => t!("command.play-pause").to_string(),
        "next" => t!("command.next").to_string(),
        "prev" => t!("command.prev").to_string(),
        "stop" => t!("command.stop").to_string(),
        "shuffle" => t!("command.shuffle").to_string(),
        "repeat" => t!("command.repeat").to_string(),
        "mute" => t!("command.mute").to_string(),
        "clear-queue" => t!("command.clear-queue").to_string(),
        "theme-system" => t!("command.theme-system").to_string(),
        "theme-light" => t!("command.theme-light").to_string(),
        "theme-dark" => t!("command.theme-dark").to_string(),
        "zoom-in" => t!("command.zoom-in").to_string(),
        "zoom-out" => t!("command.zoom-out").to_string(),
        "zoom-reset" => t!("command.zoom-reset").to_string(),
        "new-playlist" => t!("command.new-playlist").to_string(),
        "add-to-playlist" => t!("command.add-to-playlist").to_string(),
        other => other.to_string(),
    }
}

/// Indices into `cmds` whose localized label substring-matches `query`,
/// case-insensitively. An empty query matches everything, so the palette
/// shows the full list before the user types anything. Pure (and the same
/// logic the overlay's search field will drive on every keystroke): lowers
/// `query` once up front rather than per candidate.
pub(crate) fn filtered(cmds: &[PaletteCommand], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..cmds.len()).collect();
    }
    let q = query.to_lowercase();
    cmds.iter()
        .enumerate()
        .filter(|(_, c)| command_label(c.id).to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// The overlay's centered-modal width: wide enough to read a mid-length
/// command label beside its own search field without the dialog looming
/// over the window. `add_to_playlist`'s own dialog is narrower (`.w(360.0)`)
/// at that menu's pick-list proportions; this one also hosts a text field.
const DIALOG_WIDTH: f32 = 520.0;

/// The filtered list's height cap: the full registry (~20 entries) would
/// otherwise render taller than most windows, so past this the list scrolls
/// instead of the dialog growing without bound. Set directly on the
/// `.scroll()` viewport itself (not via a `.grow(1.0)` on an unbounded
/// dialog): the dialog hugs its content, so there is no ancestor height for
/// a flex-grow child to fill against.
const LIST_MAX_HEIGHT: f32 = 360.0;

/// Clamp `selected` into `[0, len)`, or `0` when `len` is `0` (nothing to
/// select). Both the renderer (so a query edit that shrinks the filtered
/// list never highlights a row that no longer exists) and the arrow-key
/// handler read the highlighted index through this, rather than trusting
/// `State::command_palette_selected` as already in range.
fn clamp_selected(selected: usize, len: usize) -> usize {
    if len == 0 { 0 } else { selected.min(len - 1) }
}

/// Move the highlighted row by one (`Key::Down`, `delta = 1`) or back one
/// (`Key::Up`, `delta = -1`), clamped to the CURRENT filtered list's bounds.
/// Re-derives `filtered` rather than trusting the last-known length: typing
/// narrows (or widens) the list out from under a stale index.
fn move_selected(s: &mut State, delta: i32) {
    let cmds = commands(s);
    let len = filtered(&cmds, &s.command_palette_query.text).len();
    if len == 0 {
        s.command_palette_selected = 0;
        return;
    }
    let current = clamp_selected(s.command_palette_selected, len);
    s.command_palette_selected = if delta < 0 {
        current.saturating_sub(1)
    } else {
        (current + 1).min(len - 1)
    };
}

/// Run command `id` (a row's click, or Enter on the highlighted one): close
/// the palette FIRST, then run its action. Order matters: "Add current
/// track to playlist" opens another overlay of its own, which closing
/// afterward would immediately undo. A no-op if `id` isn't registered (it
/// always is, for an id this module's own rows render).
fn run_command(s: &mut State, id: &str) {
    let cmds = commands(s);
    let Some(action) = cmds.iter().find(|c| c.id == id).map(|c| c.action) else {
        return;
    };
    crate::close_command_palette(s);
    action(s);
}

/// Run the CURRENTLY highlighted filtered command (Enter in the search
/// field): a no-op when the filtered list is empty, so an all-miss query
/// plus Enter does nothing instead of indexing out of range.
fn run_selected(s: &mut State) {
    let cmds = commands(s);
    let matches = filtered(&cmds, &s.command_palette_query.text);
    if matches.is_empty() {
        return;
    }
    let selected = clamp_selected(s.command_palette_selected, matches.len());
    let id = cmds[matches[selected]].id;
    run_command(s, id);
}

/// One row in the filtered list: `command_label(id)`, highlighted (a
/// `fx::SELECTED` foreground wash, the same token `views::artists`' own
/// list/grid toggle uses for its active state) when it is the row Up/Down
/// would move off of, or Enter/a click would run. `focus_selected` marks it
/// as the group's selected child for accessibility; it does not take actual
/// keyboard focus away from the search field.
fn command_row(id: &'static str, is_selected: bool, p: &Palette) -> El {
    let label = command_label(id);
    row()
        .key(format!("command-row-{id}"))
        .role("button")
        .label(label.clone())
        .focusable(true)
        .focus_selected(is_selected)
        .w_full()
        .center_y()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if is_selected {
            p.fg.alpha(fx::SELECTED)
        } else {
            Color::TRANSPARENT
        })
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(
            text(label)
                .font(type_::ITEM)
                .color(p.fg)
                .grow(1.0)
                .truncate(),
        )
        .on_press(move |st: &mut State| run_command(st, id))
}

/// The search field: bound to `State::command_palette_query`, plus the
/// arrow-key / Enter handler that drives selection and runs a command
/// without ever taking focus off the field. `on_input_key` fires on a
/// focused text field BEFORE its own editor, so returning `true` for
/// Up/Down/Enter keeps them from moving the caret or falling through to
/// Rune's own focus-group traversal.
fn search_field(s: &State, p: &Palette) -> El {
    text_input(&s.command_palette_query, type_::ITEM)
        .key("command-palette-input")
        .label(t!("command.placeholder").to_string())
        .placeholder(t!("command.placeholder").to_string(), p.text_secondary)
        .single_line()
        .w_full()
        .px(space::SM)
        .py(space::XS)
        .radius(radius::SM)
        .fill(p.muted)
        .color(p.fg)
        .on_edit(|st: &mut State| &mut st.command_palette_query)
        .on_input_key(
            |st: &mut State, ev: &rune::KeyEvent, _tx: &mut rune::Tx| match ev.key {
                Key::Down => {
                    move_selected(st, 1);
                    true
                }
                Key::Up => {
                    move_selected(st, -1);
                    true
                }
                Key::Enter => {
                    run_selected(st);
                    true
                }
                _ => false,
            },
        )
}

/// The Cmd/Ctrl-K command palette overlay, mounted at the window root (via
/// [`crate::view`]'s overlay layer): a centered, dim-backdrop modal over the
/// search field and its live-filtered command list, the highlighted row
/// moved by Up/Down and run by Enter or a click. `None` while
/// `State::command_palette_open` is `false` (closed).
pub fn overlay(s: &State, p: &Palette) -> Option<El> {
    if !s.command_palette_open {
        return None;
    }
    let cmds = commands(s);
    let matches = filtered(&cmds, &s.command_palette_query.text);
    let selected = clamp_selected(s.command_palette_selected, matches.len());

    let mut list = col()
        .key("command-palette-list")
        .role("list")
        .reveal_selected(true)
        .gap(space::XXS);
    for (row_pos, &idx) in matches.iter().enumerate() {
        list = list.child(command_row(cmds[idx].id, row_pos == selected, p));
    }

    Some(
        col()
            .key("command-palette-modal")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .fill(Color::BLACK.alpha(0.45))
            .child(
                col()
                    .key("command-palette-dialog")
                    .w(DIALOG_WIDTH)
                    .fill(p.card)
                    .radius(radius::LG)
                    .border(1.0, p.border)
                    .padding(space::LG)
                    .gap(space::MD)
                    .modal(true, crate::close_command_palette)
                    .child(search_field(s, p))
                    .child(
                        list.scroll(ScrollSpec {
                            stick_bottom: false,
                        })
                        .scroll_edge_fade(24.0)
                        .max_h(LIST_MAX_HEIGHT),
                    ),
            ),
    )
}

#[cfg(test)]
mod tests {
    use soul_player_host::PlayerHandle;

    use super::*;

    /// `overlay` is `None` while `State::command_palette_open` is `false`
    /// (the ordinary closed state no test leaves it in), the same
    /// precondition-gate shape as `views::add_to_playlist::overlay`.
    #[test]
    fn overlay_is_none_when_palette_is_closed() {
        let s = State::new(PlayerHandle::offline());
        assert!(overlay(&s, &crate::theme::palette(true)).is_none());
    }

    #[test]
    fn filtered_substring_matches_labels_case_insensitively() {
        let _locale = crate::config::use_test_locale("en");
        let cmds = commands(&State::new(PlayerHandle::offline()));

        let all = filtered(&cmds, "");
        assert_eq!(all.len(), cmds.len(), "empty query shows all");

        // "Play/Pause", "Playlists", "New playlist" all contain "play".
        let play = filtered(&cmds, "PLAY");
        assert!(!play.is_empty(), "at least one command label mentions play");
        assert!(
            play.iter()
                .all(|&i| command_label(cmds[i].id).to_lowercase().contains("play")),
            "every matched index's label actually contains the query"
        );

        assert!(
            filtered(&cmds, "zzzxxx").is_empty(),
            "a query matching nothing returns no indices"
        );
    }

    #[test]
    fn every_command_has_a_nonempty_localized_label() {
        let _locale = crate::config::use_test_locale("en");
        for c in commands(&State::new(PlayerHandle::offline())) {
            let label = command_label(c.id);
            assert!(
                !label.is_empty() && label != format!("command.{}", c.id),
                "label for {}",
                c.id
            );
        }
    }

    /// The "Add current track to playlist" command only appears once
    /// something is actually playing; every other command is always
    /// offered regardless of playback state.
    #[test]
    fn add_to_playlist_command_is_precondition_filtered() {
        let mut s = State::new(PlayerHandle::offline());
        assert!(
            !commands(&s).iter().any(|c| c.id == "add-to-playlist"),
            "nothing playing -> no add-to-playlist command"
        );

        s.now_playing_id = Some(1);
        assert!(
            commands(&s).iter().any(|c| c.id == "add-to-playlist"),
            "a playing track -> the command appears"
        );
    }

    /// Run `id`'s action against `s` and return whatever `host` recorded, so
    /// the transport/view/playlist tests below can call into the real
    /// registry (not a hand-copied closure) while still asserting its effect.
    fn run(s: &mut State, id: &str) {
        let cmds = commands(s);
        let cmd = cmds
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("{id} is registered"));
        (cmd.action)(s);
    }

    /// A nav command resets nav state exactly like a real sidebar click
    /// (`views::sidebar::nav_item`'s `on_press`): clears the back-stack and
    /// the page-scoped buffers it resets, then lands on the target route.
    #[test]
    fn nav_command_action_resets_nav_state_like_a_sidebar_click() {
        let mut s = State::new(PlayerHandle::offline());
        crate::navigate_to(
            &mut s,
            Route::Album,
            Some("Some Album".into()),
            None,
            None,
            None,
        );
        s.playlist_new_buf = rune::TextEdit::new("stale");
        assert!(
            !s.nav_history.is_empty(),
            "navigate_to pushed a back-stack entry"
        );

        run(&mut s, "nav-albums");

        assert!(matches!(s.route, Route::Albums));
        assert!(
            s.nav_history.is_empty(),
            "the sidebar-style reset clears the back-stack"
        );
        assert!(
            s.playlist_new_buf.text.is_empty(),
            "a stale buffer is reset too"
        );
        assert!(s.mobile_show_content);
    }

    /// "Go to Settings" additionally picks a deterministic tab rather than
    /// leaving whatever a previous visit selected.
    #[test]
    fn nav_settings_action_also_selects_the_appearance_tab() {
        let mut s = State::new(PlayerHandle::offline());
        s.settings_tab = SettingsTab::Audio;

        run(&mut s, "nav-settings");

        assert!(matches!(s.route, Route::Settings));
        assert_eq!(s.settings_tab, SettingsTab::Appearance);
    }

    /// Every stateless transport command sends exactly the backend `Command`
    /// its button in `views::player` sends for the same action.
    #[test]
    fn transport_command_actions_send_the_expected_backend_command() {
        let mut s = State::new(PlayerHandle::recording());
        for (id, expected) in [
            ("play-pause", Command::TogglePlay),
            ("next", Command::Next),
            ("prev", Command::Prev),
            ("stop", Command::Stop),
            ("mute", Command::ToggleMute),
            ("clear-queue", Command::ClearQueue),
        ] {
            run(&mut s, id);
            assert_eq!(
                s.host.recorded_commands().last(),
                Some(&expected),
                "command {id}"
            );
        }
    }

    /// Shuffle toggles off the current flag; repeat cycles Off -> All -> One,
    /// the same order `views::player::repeat_button` steps through.
    #[test]
    fn shuffle_and_repeat_command_actions_read_the_live_state() {
        let mut s = State::new(PlayerHandle::recording());
        s.shuffle = false;
        run(&mut s, "shuffle");
        assert_eq!(
            s.host.recorded_commands().last(),
            Some(&Command::SetShuffle(true))
        );

        s.repeat = RepeatMode::Off;
        run(&mut s, "repeat");
        assert_eq!(
            s.host.recorded_commands().last(),
            Some(&Command::SetRepeat(RepeatMode::All))
        );
    }

    /// The theme/zoom commands drive the same `set_theme_pref`/`set_zoom`
    /// helpers the Appearance settings buttons and the Ctrl +/-/0 shortcuts
    /// use, so their effect on `State` matches exactly.
    #[test]
    fn theme_and_zoom_command_actions_update_state() {
        // `set_theme_pref`/`set_zoom` persist; isolate from the real config file.
        let _g = crate::config::use_test_config_dir();
        let mut s = State::new(PlayerHandle::offline());
        s.ui_zoom = 1.0;

        run(&mut s, "zoom-in");
        assert!((s.ui_zoom - 1.1).abs() < 1e-4, "one step in");
        run(&mut s, "zoom-out");
        assert!((s.ui_zoom - 1.0).abs() < 1e-4, "back out");
        s.ui_zoom = 1.5;
        run(&mut s, "zoom-reset");
        assert_eq!(s.ui_zoom, 1.0, "resets to 100 %");

        run(&mut s, "theme-dark");
        assert_eq!(s.theme_pref, ThemePref::Dark);
        run(&mut s, "theme-light");
        assert_eq!(s.theme_pref, ThemePref::Light);
        run(&mut s, "theme-system");
        assert_eq!(s.theme_pref, ThemePref::System);
    }

    /// "New playlist" is a plain navigation to the page that owns the real
    /// inline field; "Add current track to playlist" opens the same
    /// membership-manager overlay the now-playing heart opens.
    #[test]
    fn playlist_command_actions() {
        let mut s = State::new(PlayerHandle::offline());

        run(&mut s, "new-playlist");
        assert!(matches!(s.route, Route::Playlists));

        s.now_playing_id = Some(7);
        run(&mut s, "add-to-playlist");
        assert_eq!(s.add_to_playlist_for, Some(7));
    }
}
