# Research: what the audiophile/power-user community prefers (playlist ↔ queue)

> Supporting evidence for [ADR 0001](../adr/0001-play-queue-is-a-snapshot-decoupled-from-playlists.md).
> Method: adversarially-verified deep-research pass (parallel web search → fetch →
> 3-vote refutation per claim → cited synthesis). 2026-11-21.

**Question:** What do discerning music-player communities (foobar2000 /
Hydrogenaudio, audiophiles, MusicBee / Roon / Plexamp power users) *prefer* about
the playlist↔queue relationship: do they want playlist edits to live-update the
queue, or strict separation? Including dissenting views and the strength of each
camp.

## Headline

Across every target community the preference is **strong and near-unanimous: a
play queue that is STRICTLY SEPARATE from saved playlists and behaves as a STABLE
SNAPSHOT.** Editing a playlist must not mutate what is playing, and vice-versa.
This is the opposite of the mainstream-streaming "live context" reflex.

## Verified findings (high confidence)

1. **foobar2000 doctrine:** the playlist is the editable list you manage; the
   queue is "a temporary detour, nothing else." The mechanism that live-mirrors
   queue↔playlist is explicitly **deprecated as "confusing," disabled by
   default.** A dedicated separate queue UI is the community default.
2. **MusicBee:** the stored Playlist and the "Playing Tracks" queue are
   "completely independent." A senior forum member, correcting a newcomer: add
   the song to the Playing-Tracks queue, not the playlist, "This will NOT add
   the track to your playlist **nor should it**." (An explicit value judgment.)
3. **Roon:** plays a playlist by copying it to the Queue as a detached snapshot;
   power users want the queue as an independent "sandbox," and separately want to
   see the originating playlist/album as a distinct playback-context view.
4. **Dominant grievance:** destructive queue behavior that mutates the queue
   **without an explicit action** ("Play Now" wiping the queue, played tracks
   auto-removed, "Play from here" deleting intervening tracks). They want a
   stable, persistent, **non-destructive** queue; Roon shipped exactly this to
   acclaim.
5. **Mental model:** playlist = durable, persisted document; queue = volatile,
   in-the-moment object. Users recommend building a saved playlist precisely
   because it "is persisted" and "doesn't get damaged."
6. **Power users distrust "auto-magic"** and demand explicit, predictable,
   opt-in control; they explicitly reject the Spotify-style radio-context model
   as less predictable.

## Dissent / nuance

Mainstream streaming (Spotify) users share the anti-auto-magic instinct but their
friction is different: they want *more* editability of the auto-populated
portion, which is a demand for control, not evidence for live playlist↔queue
linkage. One internal tension: findings #4 (non-destructive, played tracks
remain) vs a minority "a queue should consume songs as they play" view; resolved
by Roon's non-destructive model (played tracks stay, visible).

## Primary sources

- Hydrogenaudio: foo_queue_editor: https://wiki.hydrogenaudio.org/index.php?title=Foobar2000%3AComponents%2FQueue_Editor_%28foo_queue_editor%29
- Hydrogenaudio: "Playlists, Playback Queue…" thread 108585: https://hydrogenaudio.org/index.php/topic,108585.0.html
- MusicBee forum: thread 42491 (playlist vs Playing Tracks): https://getmusicbee.com/forum/index.php?topic=42491.0
- Roon community: currently-playing list vs queue: https://community.roonlabs.com/t/how-to-see-currently-playing-list-not-the-queue/276926
- Roon community: non-destructive queue (done in 1.4): https://community.roonlabs.com/t/non-destructive-queue-option-done-in-1-4/19694
