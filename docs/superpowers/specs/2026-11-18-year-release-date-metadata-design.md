# Year / release-date metadata — design

Date: 2026-11-18
Status: approved direction, pre-implementation
Spans two repos: `soul-audio-core` (Phase 1) and `soul-player-ng` (Phase 2).

## Goal

Surface an album's **year** in the player — "Artist • 1973" on album/artist cards and detail
headers, chronological ordering, and a "reissued" detail line — using the audiophile-correct
*original release year*, with a user setting to switch to the plain release year.

## Background & research verdict

For audiophile libraries the canonical year is the **original release date, falling back to the
main release/recording date** when the original isn't tagged. Roon defaults to it; foobar2000 /
MusicBee power-users and the MusicBrainz Picard community converge on `$if2(%originaldate%,
%date%)`; mass-market servers (Plex/Jellyfin/Navidrome) key on the plain reissue year — the gap
this feature closes. Tag precedence:

- **Original** — ID3 `TDOR`/`TORY`, Vorbis `ORIGINALDATE`/`ORIGINALYEAR`, MP4 freeform
  `----:com.apple.iTunes:ORIGINALDATE`, APE `Original Date`.
- **Fallback (main)** — ID3 `TDRC`/`TYER`, Vorbis `DATE`, MP4 `©day`, CAF `year`/`date`.

Unknown dates sort **last** and render **"—"**, never `0`/`1970`. Conflicting tags resolve by a
fixed precedence, never min/max.

### Current state (from a code audit)

- `soul-audio-core` **already** reads only the main date (`soul-audio-tags` maps `TDRC|TYER →
  Field::Date`; `soul-audio-import` derives `year = leading 4 digits of the date tag`) and bakes a
  single `u16 year` into the `.soulidx` ALBUM RECORD and `AlbumRec`, already used for
  chronological-within-artist browse ordering.
- The tag readers are **custom, hand-rolled, `no_std`** parsers (not lofty/symphonia); `Field` is
  `#[non_exhaustive]` with **no** original-date variant.
- `-ng` reads **zero** album-level dates today (`TrackView` has no date; the only `AlbumRec` access
  reads `.title`). Album order is **baked in core**; `-ng` changes order only by re-querying a baked
  axis — there is no client-side re-sort anywhere.

## Decisions (made with the user)

1. **Packed `u32` date** in the index (not a bare `u16` year): exact chronological sort + a
   "reissued YYYY-MM-DD" detail line.
2. **Store both dates** per album (original + release) so the toggle switches with **no rescan**.
3. **Settings toggle "Album year: Original | Release"**, default **Original**. It **re-labels the
   displayed year everywhere** and **re-orders the Artist-page Discography** live (client-side).
   The global Albums grid keeps its core-baked artist-grouped order, re-labeled.
4. Core's baked chronological order keys on the **original** date (the default); `release` ordering
   is achieved by `-ng` re-sorting the discography client-side from the two dates it carries.

## Data model

### `PackedDate(u32)` — new, in `soul-audio-core` (`no_std`, pure integer)

```
value = (year << 9) | (month << 5) | day
  day:   bits 0..4   (1–31; 0 = unknown)
  month: bits 5..8   (1–12; 0 = unknown)
  year:  bits 9..31  (0 = unknown)
  0 overall = fully unknown.
```

- Pack/unpack are integer bit-ops only — no float, no `libm`, `no_std`/no-alloc safe (it ships in the
  thumbv7em DAP binary via the shared index reader).
- Accessors: `year() -> u16`, `month() -> u8`, `day() -> u8`, `is_known() -> bool`.
- **Sort key:** ascending `value` is naturally chronological (year dominates). Unknown (`0`) must
  sort **last**, so callers use `sort_key() = if value == 0 { u32::MAX } else { value }` — mirroring
  the existing `year_at` `0 → u16::MAX` rule.
- Home crate: `soul-audio-catalog` (where `AlbumRec` lives, already `no_std`), re-exported as needed.

### `.soulidx` ALBUM RECORD, format v4 → v5

Record grows **24 → 30 bytes**; the single `14 year u16` becomes two packed dates; `folder` and
`art_id` shift right:

```
 0 id u32 · 4 title.off u32 · 8 title.len u16 · 10 album_artist u32 ·
14 original_date u32 · 18 release_date u32 · 22 folder u32 · 26 art_id u32
```

- `FORMAT_VERSION: u16` **4 → 5** (`soul-audio-index/src/lib.rs:106`). The reader's version gate
  (`:166`) rejects v4 and the desktop rebakes — one-way, migration-free (same as the v4 mbid / v2
  sort-name bumps). A v5 `.soulidx` already on a DAP card must be re-baked.
- `ALBUM_RECORD_SIZE: usize` **24 → 30** (`:110`); all section-offset math cascades from it.
- **Riskiest edit (no compile guard):** the reader (`:341-359`) and baker (`:852-863`) hardcode
  intra-record offsets. Reader `:355 year @14` → two `le_u32` reads @14 and @18; **`:356 folder`
  16→22** and **`:357 art_id` 20→26**. Baker `:859` year write → two `u32` writes; the following
  folder/art_id writes keep order and the `debug_assert_eq!(out.len(), artists_off)` at `:864`
  catches a size slip. A unit test asserting read-back of a two-date album record guards this.

### Tag readers — add `Field::OriginalDate`

`soul-audio-tags` (`no_std`). `Field` enum (`lib.rs:41-80`) gains an `OriginalDate` variant after
`Date`. The in-crate exhaustive match in the owned collector must handle it. Per-container mapper
additions (one line each):

| Format | File:line | Add |
|---|---|---|
| ID3v2 | `id3v2.rs:245` | `"TDOR" \| "TORY" => Some(Field::OriginalDate)` (plain text path) |
| Vorbis/FLAC/OGG/Opus | `vorbis.rs:35` | `ORIGINALDATE` / `ORIGINALYEAR` (one mapper serves all) |
| MP4/iTunes | `mp4.rs:330` | freeform `----:com.apple.iTunes:ORIGINALDATE` / `ORIGINALYEAR` |
| CAF | `caf.rs:155` | `original date` / `original year` |
| APE (WavPack) | `wavpack.rs:135` | `Original Date` / `Original Year` |
| Matroska/WebM | `matroska.rs:294` | `DATE_RECORDED` (vs the existing `DATE`/`DATE_RELEASED`) |
| RIFF/WAV | — | no standard original fourcc; skipped |
| AIFF / DSF / DFF | — | inherit via their embedded ID3v2; no change |

Owned collector (`owned.rs`): add `pub original_date: Option<String>` to `Tags` (`:39`) and a
`Field::OriginalDate => self.tags.original_date.get_or_insert_with(..)` arm (`:101`, first value
wins — same rule as `date`).

### Import — derive two dates, bump the scan cache

`soul-audio-import` (`std`):

- Mirror the existing year parse (`lib.rs:286-292`, "leading 4 digits") into a shared helper that
  parses a tag string to a `PackedDate` (leading ISO `YYYY[-MM[-DD]]`; plausible-year guard — a
  **fixed** `1000..=2200` range, no clock, so parsing is deterministic and test-stable — else
  unknown; month out of `1..=12` or day out of `1..=31` → that component unknown). Apply it to
  **both** `tags.original_date` and `tags.date`.
- The album's two dates follow the existing **"first interned track wins"** reduction
  (`Interner::album`, `lib.rs:179-203`) — unchanged behavior, applied to each date independently.
  `RawTrack` gains the second parsed date; `AlbumMeta` carries `original_date` + `release_date`.
- **`SCAN_CACHE_VERSION` 2 → 3** (`lib.rs:429`): `RawTrack`/`Tags` shape changes, so warm
  `scan_caches.bin` must be discarded, not reused with an absent second date.
- `soul-audio-organize/template.rs` is **out of scope** (its `{Year}` filename token keeps the
  main-date behavior; an `{OriginalYear}` token is a deferred nice-to-have).

### Catalog / ordering

- `AlbumRec` (`soul-audio-catalog/src/lib.rs:95-102`): `year: u16` → `original_date: PackedDate` +
  `release_date: PackedDate`. Keeps `Copy`/`Default`/`Eq`.
- `AlbumMeta` (index build input, `:515-520`): same two fields.
- **Baked order keys on `original_date`.** `year_at` (`soul-audio-index/src/lib.rs:714-723`, used
  `:733`/`:766`) and the multicatalog merge key `Head.year` (`soul-audio-multicatalog`
  `:193-197`/`:255`/`:284`) must both switch to `original_date.sort_key()` **identically**, or the
  desktop merge diverges from the device-frozen order.

## `-ng` (Phase 2)

### Backend projection

- Add an album-date resolver beside `album_title` (`soul-player-backend/src/library.rs:361`):
  `album_dates(&cat, album_id) -> (original: u32, release: u32)` reading the new `AlbumRec` fields
  (as raw packed `u32`, so the DTO stays `no_std`-agnostic plain integers over the wire).
- `TrackView` (`types.rs:224-237`) gains `original_date: u32` and `release_date: u32` (packed;
  `0` = unknown), populated in `views_by` (`library.rs:296`) from the track's album. This carries
  the album's dates on every track row — accepted duplication; no album-level DTO exists in the flat
  `Vec<TrackView>` host contract and adding one is larger net-new surface. All ~12 `TrackView`
  constructor sites (tests/synthesis) set the two new fields (default `0`).

### Config / state / setting

- `config.rs`: a `Config` field `album_year_source: AlbumYearSource` (enum `Original | Release`,
  `#[serde(default)]`, default `Original`) with a `load_album_year_pref` / `save_album_year_pref`
  pair, mirroring `load_theme_pref`/`save_theme_pref`.
- `main.rs`: `State.album_year: AlbumYearSource` (loaded at startup), a `set_album_year` setter
  (mutate state + `config::save_album_year_pref`).
- `settings.rs`: a two-button segmented control in the **Appearance** card, modeled on
  `theme_button` (active = `p.primary`, `on_press → set_album_year`).

### Display & sort

- **Selected date** = `album_year == Original ? original_date : release_date`; when the selected one
  is unknown, fall back to the **other** date; when both are unknown, "—"/omitted. The displayed
  value is that date's `year()`. Helper on `State` (or a free fn) over a grouped `Album`'s two dates.
- `Album` (`albums.rs:23-28`) carries the two packed dates (picked up in `group_albums` from the
  member tracks, first-wins).
- **Display sites:** album card subtitle `albums.rs:96` → "Artist • 1973" (artist alone when
  unknown); album detail hero `albums.rs:286` + discography rows/cards (`artists.rs:294`/`:344`);
  a "reissued {release}" line via the shared hero `detail_page.rs:79-88` when both dates are known
  and differ.
- **Discography re-sort:** `albums_of_artist` (`albums.rs:69-83`) sorts the grouped albums by the
  **selected** date's `sort_key` ascending (unknown last). Re-runs each render, so flipping the
  setting re-orders live — no rescan. The global Albums grid (`albums_page` → `group_albums`) keeps
  the baked artist-grouped order, re-labeled only.

### Core bump & lockfiles

Merge Phase 1 to core `main`, then in `-ng`:
`cargo update -p soul-audio-core --precise <new SHA>` on `app/rune/Cargo.lock` **and**
`crates/soul-player-host/Cargo.lock` (both pin the git SHA today). **Reconcile the root
`Cargo.lock`** — in its committed form it records the core crates with **no git source line** (not
SHA-pinned), so regenerate/reconcile it rather than `--precise`. CI legs `rust` (root + host) and
`rune` (app/rune) gate the bump.

## Error handling & edge cases

- Unknown date → `PackedDate(0)`; displays "—"/omitted, sorts last (`sort_key → u32::MAX`).
- Partial dates: `YYYY` or `YYYY-MM` keep month/day = 0; display collapses to the year; sort uses
  what's present.
- Implausible year (out of the fixed `1000..=2200`) → unknown.
- Conflicting tags on one file → fixed precedence (OriginalDate vs Date are **separate** fields; no
  averaging). Album reduction is first-interned-track-wins (existing behavior, per date).
- `original_date` absent (the common case) → the "Original" setting shows the release year via the
  fallback; no empty state for well-tagged libraries.

## Testing

**Core (Phase 1):**
- `PackedDate` pack/unpack/sort_key unit tests (incl. unknown → `u32::MAX`, partial dates).
- Per-container tag tests: a `DATE`+`ORIGINALDATE` (or `TDRC`+`TDOR`, `©day`+freeform, APE, CAF,
  Matroska) fixture emits both `Field::Date` and `Field::OriginalDate` — following the existing
  ReplayGain multi-field fixture pattern (`id3v2.rs:1022`, `owned.rs:391`).
- Import: date-string → `PackedDate` helper (mirrors `template.rs:207` year-token test); a scanned
  album carries both dates (extends `roundtrip.rs:160 album.year == 2012`).
- Index roundtrip: a baked two-date album record reads back exactly (guards the offset shift);
  `FORMAT_VERSION == 5`.
- Multicatalog: merged order matches the baker's `original_date` key (extends the year-ordering
  tests at `multicatalog.rs:418+`).
- Keep green: `architecture-tests/no_alloc.rs` (reader path), `import/incremental.rs:97`
  byte-identity.

**`-ng` (Phase 2):**
- Backend projection: `TrackView` carries the album's two dates.
- Config load/save round-trip for `album_year_source`; default `Original`.
- Render: album card shows "Artist • {year}"; unknown → artist alone.
- Toggle: a seeded library with a reissue (original ≠ release) — switching `album_year` changes the
  displayed year AND re-orders the discography (assert card order via element rects, à la the grid
  tests); the Albums grid order is unchanged.
- i18n: any new strings (e.g. "reissued", the setting label/options) in all 6 locales.

## Sequencing

1. **Phase 1 — `soul-audio-core`** (one PR): `PackedDate`; tag `Field::OriginalDate` + per-container
   mappers + owned `Tags.original_date`; import dual-date derivation + `RawTrack`/`AlbumMeta` +
   `SCAN_CACHE_VERSION` 3; index v5 record + reader/baker + `AlbumRec`; `year_at` + multicatalog on
   `original_date`; all tests. Merge → capture new core SHA.
2. **Phase 2 — `-ng`** (one PR): bump core SHA across the three lockfiles (reconcile root); backend
   `album_dates` + `TrackView` dates; config/state/setting; display + discography re-sort; tests.

## Risks

- **Reader offset shift** (`index lib.rs:355-357`) — highest; silent on-device mis-read if missed.
  Guarded by a roundtrip read-back test.
- **Order-key consistency** — `year_at` and multicatalog `Head` must key on the same date, or the
  merge diverges from the frozen device order.
- **`SCAN_CACHE_VERSION` bump** — omit it and warm caches serve the absent second date.
- **`FORMAT_VERSION` v5 is one-way** — existing v4 cards/indexes rebake on next scan.
- **Root `Cargo.lock` not SHA-pinned** — the three-lock bump isn't uniform; reconcile the root.

## Out of scope / deferred

- `{OriginalYear}` organize/filename token.
- Per-track (vs per-album) dates; compilation-aware album-year aggregation beyond first-wins.
- Sorting the global Albums grid by date (kept artist-grouped/baked).
- A Roon-style "reissued" badge beyond the single detail line.
