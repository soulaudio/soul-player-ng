# UI parity reference: old Soul Player vs the Rune `-ng` head

Reference screenshots and findings for driving the `-ng` Rune UI toward 1:1 with
the original React/Tauri Soul Player (`D:/dev/soulaudio/soul-player`,
`applications/{shared,desktop}`).

## How these were captured

Both apps were driven over the **same library**
(`D:/dev/soulaudio/testing-audio-library`: 13 albums / 9 artists / 68 tracks).

- **Old app** (`old/`): the Tauri desktop app run live (`yarn workspace
  soul-player-desktop tauri:dev`), driven over the WebView2 CDP endpoint with
  `playwright-core` (`chromium.connectOverCDP`). Launch it on a dedicated debug
  port: `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333`.
  (Gotcha: the old app's backend fails `AppState` init against an
  old-schema DB; delete/rename `%APPDATA%/Soul Player Dev/soul-player.db` so it
  creates a fresh current-schema DB, then it re-scans the configured library.)
- **-ng** (`ng/`): rendered headlessly via
  `SOUL_PLAYER_FOLDER=<lib> soul-player-rune --snap out.png --size WxH --scene <name> --theme dark`
  (delete `%LOCALAPPDATA%/soul-player/logs/running.lock` before each shot to
  avoid the crash-recovery banner).

A rendered side-by-side lives in `parity-report.html`.

## Findings (old = reference, what we match toward)

Shared bones: sidebar + dot-nav, "NOW PLAYING" eyebrow, bottom transport,
lavender accent, single-pane collapse at narrow width. The gap is **richness**.

| Page | Old (reference) | -ng (ours) |
|---|---|---|
| Albums | search + Filters→genre chips; 5-col; cover art all loaded; subtitle = artist | no search/filters; 7-col denser; subtitle = "Artist · Year"; several covers missing |
| Album detail | full track table: # / Title / Artist / Album / **format badge** / Duration + headers; Play All + queue btn | minimal # / Title / Duration; Play All only; placeholder cover |
| Artists | **blurred-art circular avatars** + person icon; search; "N albums · M tracks" | flat placeholder circles; no search; "N tracks" |
| Genres | card-tile grid + search | plain text list rows |
| Home | discovery: "Jump back into" + "Do some crate digging" | library overview: "Your Library" stats + Play All + album grid |
| Tracks | columned table + **color-coded codec format badges** + headers | search + sort toggle (Artist/Album/Title/Folder) + inline "+" add; no format badges |
| Playlists | (empty state in capture) | (empty state) |

Interactions (old): album card **hover → play ▶ overlay** + title opacity; click
cover = play, click title = detail; **right-click album → Add to Playlist /
Edit Artwork**; **Filters → genre chips**. Responsivity: both collapse to a
single sidebar-first pane at ~560px.

## Parity backlog (toward 1:1)

**High**
1. Cover/avatar **art extraction** — many album covers + most artist avatars are placeholders in -ng. Biggest visual gap.
2. **Album-detail track table** — add Artist / Album / **format badge** columns + headers.
3. **Search** on Albums / Artists / Genres grids.
4. **Artist avatars** — blurred-album-art background + "N albums · M tracks".

**Medium**
5. **Filters** (genre chips) on Albums.
6. **Format badges** on the Tracks table (color-coded per codec) + headers.
7. **Hover play-overlay** on album/artist cards.
8. **Genres** presentation — card-tile grid vs our text list (decision).
9. **Home** concept — discovery vs library-overview (decision).

**Low**
10. **Nav order** — old is …Artists / Playlists / Genres / Tracks; -ng swaps Genres/Playlists.
11. **Grid density** — old 5-col (larger cards) vs -ng 7-col; tune card min-width.
12. **Detail secondary action** — queue/add button beside Play All.
