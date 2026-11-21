# Heart -> Add-to-Playlist + Membership Manager

**Status:** approved (model chosen by user)
**Date:** 2026-11-19
**Builds on:** playlists (#229)

## Goal

Make the now-playing heart icon functional: clicking it opens the add-to-playlist
dialogue for the currently playing track. Upgrade that dialogue from add-only
into a membership manager: it lists the user's playlists with a checked state for
the ones already containing the track, and ticking/unticking a row adds/removes
the track immediately. This restores (and cleans up) the original soul-player's
heart behavior, whose heart tooltip was literally "Add to Playlist" and opened a
generic add/remove-membership dialogue.

## Why this shape

Research into the original soul-player found its heart was never a per-track
"like" and there was no Liked-Songs concept or separate Favorites page: the heart
opened a generic add-to-playlist dialogue (checkbox list, diffed on save). The
user chose to stay faithful to that: heart -> dialogue, dialogue manages
membership, and the already-shipped Playlists nav page + its Settings ->
Navigation show/hide toggle are the "page in menu". No Liked-Songs special
playlist, no new nav page.

App-only: no `soul-audio-core`, `soul-player-backend`, or `soul-player-host`
change. The existing `PlaylistStore` API (`add_entry`, `remove_entry`, `get`,
`list`) and `host.locator_of` are sufficient.

## Behavior

- **Heart (now-playing panel):** the muted decorative `icons::HEART` at
  `player.rs` becomes an interactive button (role/focusable/cursor/hover/press),
  labelled "Add to playlist", calling `open_add_to_playlist(s, track_id)` for the
  current track. Shown only when something is playing (the empty state already
  omits the heart). The heart has no filled/liked state (there is no like
  concept); it is an action trigger.
- **Dialogue = membership manager** (the existing `views::add_to_playlist`
  overlay, opened from both the heart AND the existing library context menu):
  - Lists every playlist (`s.playlists.list()`). Each row shows a checked mark
    when the track is already in that playlist (`already_has` by locator key).
  - Clicking a row **toggles membership immediately**: not present -> add
    (`add_track_to_playlist`); present -> remove (a new
    `remove_track_from_playlist` that finds the entry by locator key and calls
    `store.remove_entry`). The overlay stays open (it is no longer a pick-one-and-
    close picker).
  - "New playlist..." inline field: create + add the track + keep open with the
    new row checked.
  - A "Done" affordance (plus the existing backdrop dismiss) closes the overlay.
- **The now-playing track snapshot:** `add_track_to_playlist` builds the snapshot
  from the `TrackView` in `s.library`. The now-playing track must resolve there;
  if the now-playing id is not in `s.library` (edge case), fall back to the
  resolved `s.track(id)` for the snapshot, or no-op gracefully (never panic).

## Out of scope

Liked-Songs / per-track like toggle and a filled-heart state (the rejected Model
B); a separate Favorites page; a per-track-row heart (the heart stays in the
now-playing panel only); diff-on-save semantics (we apply each toggle
immediately, which the store persists per mutation).

## Testing (TDD)

- Heart renders as a button when playing and opens the overlay for the current
  track (`add_to_playlist_for == Some(now_playing_id)`); absent/no-op when
  nothing is playing.
- Membership row shows checked for a playlist containing the track; toggling an
  unchecked row adds + persists; toggling a checked row removes + persists; the
  overlay stays open across toggles.
- `remove_track_from_playlist` removes the right entry by locator (not by a
  stale/ guessed index) and is a no-op when the track is not present.
- App render e2e: play a track -> click the heart -> dialogue opens -> tick a
  playlist -> it is added -> untick -> it is removed.

## Global constraints

- App-only: modify only `app/rune` (`views/player.rs`, `views/add_to_playlist.rs`,
  possibly `main.rs`, `locales/app.yml`, `i18n_tests.rs`). No backend/host/core.
- No em-dashes; no attribution in commits; strings localized in 6 locales.
- The dialogue is the existing themed overlay (not a native picker).
- TDD; app/rune builds on nightly (auto from CWD); clippy `--all-targets`.
