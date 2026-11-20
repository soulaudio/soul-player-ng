# Command Palette Design

**Status:** approved (autonomous drive; design validated against rune APIs)
**Date:** 2026-11-19
**Issue:** #103 (Command palette & keyboard shortcuts)

## Goal

A Ctrl/Cmd-K command palette: a centered overlay that fuzzy-filters the app's
actions and navigation, letting the user jump anywhere or run any command from
the keyboard. App-only (`app/rune`); no backend/host/core.

## Behavior

- **Open:** Cmd-K (macOS) / Ctrl-K (elsewhere), from anywhere, via
  `ev.is_command('k')` in the global `handle_key`.
- **Overlay:** a centered modal (mirrors `views::add_to_playlist::overlay`): an
  autofocused query field on top, a scrolled list of matching commands below with
  the current selection highlighted. Esc and click-outside dismiss it (free from
  `El::modal`).
- **Filter:** case-insensitive substring over each command's localized label
  (mirrors `views::library::track_matches`' convention: lowercase the query once,
  `label.to_lowercase().contains(&q)`). Empty query shows all commands.
- **Keyboard:** while the query field holds focus, `El::on_input_key` on it
  handles Up/Down (move the selection, clamped to the filtered list) and Enter
  (run the selected command), returning `true` to consume those; all other keys
  (typing) pass through to the editor (return `false`) and reset the selection
  to the top match (the query re-ranks the list).
- **Run:** running a command (Enter or click) first CLOSES the palette, then
  invokes the action (some actions, e.g. "Add to playlist", open another overlay,
  so the palette must be closed first). After a query edit, the selection clamps
  back to 0 so Enter always targets a visible row.

## Commands (v1)

A registry `&[PaletteCommand { id: &'static str, action: fn(&mut State) }]`.
Labels are localized via a `match id -> t!("command.<id>")` (dynamic `t!` keys
are disallowed, so the label mapping is an explicit match, like `nav_label`).

- **Navigate:** Home, Albums, Artists, Genres, Playlists, Tracks, Settings
  (`s.route = Route::X` + the sidebar's reset, mirroring `sidebar.rs`'s nav
  `on_press`; Settings also `set_settings_tab`).
- **Transport:** Play/Pause, Next, Previous, Toggle shuffle, Cycle repeat,
  Mute toggle, Clear queue (`s.host.send(Command::...)` / `toggle_mute`).
- **View:** Theme System / Theme Light / Theme Dark (`set_theme_pref`), Zoom in,
  Zoom out, Reset zoom (`set_zoom`/`stepped_zoom`).
- **Playlist:** New playlist (navigate to Playlists + open the inline field, or
  create a default-named one), Add current track to playlist (only when something
  is playing -> `open_add_to_playlist(s, now_playing_id)`).

Commands whose preconditions aren't met (e.g. Add-current when nothing plays) are
omitted from the list for that render.

## Design (where things live)

- **State** (`main.rs`): `command_palette_open: bool`, `command_palette_query:
  rune::TextEdit`, `command_palette_selected: usize`. Helpers
  `open_command_palette(s)` (sets open, `query = TextEdit::new("")` with
  `query.focused = true`, `selected = 0`) and `close_command_palette(s)` (clears
  all three), mirroring `open/close_add_to_playlist`.
- **Trigger** (`handle_key`, `main.rs`): a branch near the top -
  `if ev.is_command('k') { open_command_palette(s); return true; }`.
- **Overlay** (`app/rune/src/views/command_palette.rs`, new): `pub fn overlay(s,
  p) -> Option<El>` gated on `command_palette_open`; mounted via
  `overlays.extend(views::command_palette::overlay(s, &p));` at the overlay-root
  (`main.rs`, beside the other overlays). Builds the filtered+selected list;
  `command_label(id) -> String`; the registry lives here.
- **Filter + selection** are pure (unit-testable): `filtered_commands(all,
  query) -> Vec<&PaletteCommand>` and the clamp logic.
- **i18n:** `command.<id>` keys for every command, in all 6 locales.

## Out of scope (v1)

Fuzzy ranking / subsequence scoring (substring only); searching tracks / albums /
artists as palette results (navigation only); command recency/history; a separate
keyboard-shortcuts help sheet; customizable keybindings. (The app already has
transport/volume/zoom shortcuts in `handle_key` - unchanged.)

Auto-scrolling the keyboard selection into view is also deferred: `reveal_selected`
only takes effect on a virtualized list, and the palette's list is a plain column,
so arrowing past the visible rows of the full (unfiltered) list moves the highlight
below the fold without scrolling. The autofocused query field makes type-to-filter
the primary path (which keeps the active set short), so this is a follow-up (wire
the list through virtual rows, or track a manual scroll offset), not a v1 blocker.

## Testing (TDD)

- **Pure:** `filtered_commands` substring-filters by label (case-insensitive,
  empty = all); selection clamps to `[0, filtered.len())`.
- **Trigger:** `handle_key` with a synthetic `is_command('k')` event opens the
  palette (`command_palette_open` true) and consumes (returns true).
- **Overlay render:** when open, the overlay renders (query field + command rows);
  when closed, `overlay()` is `None`.
- **Keyboard nav (full-app harness):** open palette -> type to filter -> Down
  moves the selection -> Enter runs the selected command (assert its effect, e.g.
  route changed) and closes the palette. Escape/click-outside closes.
- **Run via click:** clicking a command row runs it + closes.
- **Locale completeness:** every `command.*` key present in all 6 locales.

## Global constraints

- App-only: `app/rune` (`main.rs`, `views/command_palette.rs`, `locales/app.yml`,
  `i18n_tests.rs`, tests). No backend/host/core.
- NO em-dashes; NO attribution in commits; 6 locales for every command label.
- The overlay is the themed `El::modal` pattern (not a native picker).
- Keyboard-nav tests use the full-app `main.rs` harness (`Runtime::new(s, view,
  ..)` + real key/pointer events), since the panel-only harness does not route
  events. TDD; clippy `--all-targets`.
