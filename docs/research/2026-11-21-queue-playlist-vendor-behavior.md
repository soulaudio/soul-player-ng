# Research: how players handle a playing playlist ↔ the up-next queue (vendor behavior)

> Supporting evidence for [ADR 0001](../adr/0001-play-queue-is-a-snapshot-decoupled-from-playlists.md).
> Method: adversarially-verified deep-research pass (parallel web search → fetch →
> 3-vote refutation per claim → cited synthesis). 2026-11-21.

**Question:** When you add/remove/reorder a track in a playlist you are currently
playing, does the up-next queue update — and how do established players model the
relationship? (Spotify, Apple Music, YouTube Music, Tidal, Amazon Music,
foobar2000, MusicBee, Plexamp/Plex, Roon, Clementine/Strawberry.)

## Verified findings (high confidence unless noted)

1. **Two-layer queue is universal.** A CONTEXT queue (auto-generated from the
   playlist/album/artist you played) + a separate MANUAL queue (explicit
   "Add to queue"). Spotify: "Next from" (context) vs "Next in queue" (manual).
2. **Manual queue has priority:** `current → manual-queued → context remainder`.
3. **Apple Music** separates by landing position ("Play Next" = top, "Add to
   Queue" = end) and **preserves the source context**: insert an album, and when
   it ends Music resumes the original playlist.
4. **Plex** is the most explicit model: EXPLICIT queue (played the playlist/album
   whole) vs IMPLICIT queue (played one track, the rest "come along for the
   ride"). Adding to the queue shifts existing items, never deletes them.
5. **Snapshot vs live is genuinely split** (lower confidence — the crux):
   MusicBee and Spotify's Web API treat the context as a **frozen snapshot**
   (edits don't propagate); Spotify's native client is *reported* to reflect
   edits (inferred, not documented). A frozen snapshot is a real, shipped
   convention.
6. **Current-track interruption:** diverges. Old iTunes stops playback when the
   current track is removed (an AppleScript works around it); foobar2000 keeps
   playing. The robust, least-surprising choice is to never restart the current
   track.

## Caveats

The finding most central to the question (does a live edit propagate?) rests on
weaker sources (search snippets, a developer-forum/GitHub issue about the Web
API, not the native UX). Reorder-while-playing is essentially undocumented across
all sources. Spotify's queue UI has changed repeatedly and now exposes up to four
sections.

## Primary sources

- Spotify — Play Queue: https://support.spotify.com/us/article/play-queue/
- Spotify — How the Play Queue works (FAQ): https://community.spotify.com/t5/FAQs/How-does-the-Play-Queue-work/ta-p/1308142
- Apple — Queue your songs (Mac): https://support.apple.com/guide/music/queue-your-songs-musb1e6d1c76/mac
- Apple — Use the Playing Next queue: https://support.apple.com/guide/music/use-the-playing-next-queue-musb1e6d1c76/1.1/mac/11.0
- Spotube feature request (Priority vs Automatic queue): https://github.com/team-spotube/spotube/issues/3216
