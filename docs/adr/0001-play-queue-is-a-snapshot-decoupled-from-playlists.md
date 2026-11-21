# ADR 0001: The play queue is a stable snapshot, decoupled from playlists

- **Status:** Accepted
- **Date:** 2026-11-21
- **Deciders:** project maintainer
- **Enforced by:** `app/rune` architecture test `editing_a_playing_playlist_does_not_mutate_the_queue` (+ `..._on_remove`), see [docs/ARCHITECTURE.md](../ARCHITECTURE.md) › Enforcement.

## Context

When you play a playlist, the app resolves its entries to track ids and sends a
single `Command::PlayContext { ids }`; the player's queue becomes that id list.
The queue has no memory that it came from a particular playlist, and editing a
playlist (`add_track_to_playlist`, remove, reorder) does not touch the queue.

The open question was: **if you add a track to a playlist you are currently
playing, should the up-next queue update to include it?** This is the
"live-context" model (the playlist is a live source the queue mirrors) versus the
"snapshot" model (playing captures a detached copy; later edits don't disturb
playback). We researched both how established players behave and what our target
community actually prefers.

### What established players do (vendor behavior)

- Every serious player splits up-next into a **context queue** (auto-generated
  from the playlist/album you played) and a **manual queue** (tracks you
  explicitly "Add to queue"), with the manual queue taking priority:
  `current → manual → context remainder`. Spotify labels these "Next from" /
  "Next in queue" ([Spotify Play Queue](https://support.spotify.com/us/article/play-queue/)).
- The context carries an **identity**: Apple Music resumes the original playlist
  after an inserted album finishes ([Apple: Queue your songs](https://support.apple.com/guide/music/queue-your-songs-musb1e6d1c76/mac));
  Plex distinguishes an "explicit" from an "implicit" queue.
- **Snapshot is a shipped, mainstream convention**, not a deficiency: MusicBee
  keeps the stored playlist and the live "Playing Tracks" queue independent;
  Spotify's Web API treats the context as frozen. Live edit-propagation is a
  streaming-client refinement, not a universal expectation.
- When a queue is refreshed, the robust choice is foobar2000's: never interrupt
  or restart the currently-playing track.

### What our target community prefers (and it is lopsided)

Our audience is the audiophile / power-user crowd (foobar2000, MusicBee, Roon),
and the preference there is strong and near-unanimous:

- **foobar2000 doctrine** (Hydrogenaudio): the playlist is the editable list you
  manage; the queue is "a temporary detour, nothing else." The mechanism that
  live-mirrors queue↔playlist is explicitly deprecated as "confusing," disabled
  by default ([HA foo_queue_editor](https://wiki.hydrogenaudio.org/index.php?title=Foobar2000%3AComponents%2FQueue_Editor_%28foo_queue_editor%29),
  [HA thread 108585](https://hydrogenaudio.org/index.php/topic,108585.0.html)).
- **MusicBee** (forum, a senior member correcting a newcomer): the stored
  Playlist and the Playing-Tracks queue are "completely independent… editing the
  Playlist has no effect on what will be played," and queue edits should not
  touch the playlist — "nor should it"
  ([MusicBee thread 42491](https://getmusicbee.com/forum/index.php?topic=42491.0)).
- **Roon** plays a playlist by copying it into the Queue as a detached snapshot;
  power users want the queue as an independent "sandbox"
  ([Roon community](https://community.roonlabs.com/t/how-to-see-currently-playing-list-not-the-queue/276926)).
- The loudest, most unanimous grievance across these communities is a queue
  being mutated or wiped **without an explicit user action**. They want more
  stability and explicit control, and they reject the streaming "radio context"
  mental model as unpredictable "auto-magic."

Full research reports (two adversarially-verified deep-research passes, ~200
agents, primary sources) are archived under `docs/research/` alongside this ADR.

## Decision

**The play queue is a stable snapshot that is decoupled from saved playlists.**

1. Playing a playlist resolves its entries to a detached id snapshot
   (`PlayContext`). The queue does not hold a live reference to the playlist.
2. Editing a playlist you are currently playing (add / remove / reorder) **does
   not** mutate the active queue. No playlist mutation path issues a playback or
   queue command.
3. To put a track into up-next deliberately, the user takes an **explicit**
   action ("Add to queue" / "Play next" → `Command::Enqueue` / `PlayNext`),
   which is separate from editing the playlist.
4. The queue is **non-destructive and explicit**: nothing mutates or clears it
   except a deliberate transport/queue command. (This is the community's
   strongest positive ask and the direction to invest in, distinct from this
   decision but born of the same principle.)

This matches the mental model our audience already holds: the **playlist is the
durable document; the queue is the in-the-moment playback object**, and the two
are independent.

## Consequences

**Positive**

- Aligns with the stated preference of the target community (foobar2000 /
  MusicBee / Roon) and with those players' actual designs.
- Predictable: a playlist edit never surprises you by changing what plays next;
  the currently-playing track is never interrupted or restarted.
- The behavior we already ship (frozen id-snapshot) is correct — no
  playlist→queue coupling to build or maintain.
- Keeps the layering clean: the player/queue (host/backend/core) stays unaware
  of the editable playlist store (`app/rune`).

**Negative / trade-offs**

- Differs from the Spotify-native reflex some users have ("I added it to the
  playlist I'm playing, why isn't it next?"). Mitigated by the explicit
  "Add to queue" affordance.
- Reorder/remove of the playing playlist likewise won't reshape the queue; the
  snapshot is intentionally stable.

## Alternatives considered

- **Live-context re-projection** (queue mirrors the playlist live, non-
  interrupting). Rejected: it is the model our target community explicitly
  dislikes, it adds cross-repo complexity (a non-disruptive context-source
  update in core), and the vendors that do it are mainstream streaming clients,
  not the audiophile players we emulate.
- **Enqueue the added track automatically.** Rejected: semantically wrong (a
  track appended to a playlist's end would jump to play-next), and it is still
  implicit auto-magic the community distrusts.
