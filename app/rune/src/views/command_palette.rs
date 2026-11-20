//! The Cmd/Ctrl-K command palette: model + pure logic. A flat registry of
//! named actions (go to a page, transport, view, playlist) that can be run
//! without the mouse, plus the substring filter the overlay's search field
//! drives. This task wires the registry, the filter, and the localized
//! labels; the overlay itself (its mount, list rendering, and arrow-key
//! navigation) is a later task: `main.rs`'s `open_command_palette` /
//! `close_command_palette` only flip the flag this module's callers will
//! read.
#![forbid(unsafe_code)]
// This task wires the registry (`commands`/`command_label`/`filtered`) and
// every action it lists, but nothing in production code calls into them
// yet: the overlay that renders the filtered list and runs the highlighted
// command's `action` is Task 2. Until then this whole module is genuinely
// unreached outside its own tests, same shape as `playlists::mod`'s own
// module-wide allow while an earlier task's wiring was still incomplete.
#![allow(dead_code)]

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

#[cfg(test)]
mod tests {
    use soul_player_host::PlayerHandle;

    use super::*;

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
