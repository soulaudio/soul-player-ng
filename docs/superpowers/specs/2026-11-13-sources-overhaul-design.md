# Sources Overhaul — Design Spec

**Status:** Draft for review · **Date:** 2026-11-13 · **Repo(s):** soul-player-ng (+ a new crate in soul-audio-core for Phase 2)

## Background

Today the **Music Data** settings card is a flat list: `+ Add Source` (native
folder dialog → `host.add_folder`), one row per folder (basename + "offline"
badge + ✕ remove), and a Rescan button. It binds 1:1 to the host contract
`add_folder / remove_folder / set_folders / watched_folders → WatchUpdate { views,
unavailable }`. All sources are **reference-only**: folders are scanned in place
and never modified.

The original soul-player (React/Tauri) had a much richer model — watched folders
(with enable, sync-deletes, per-source rescan), a **managed library** that
physically organizes imported files into a `{AlbumArtist}/{Album}/…` template
(copy/move), external-file routing, and remote **server sources** (shown there as
"Coming Soon"). We want to bring the useful parts of that to the Rune head
*without* betraying the architecture that makes the core DAP-ready.

## Goal

Make the Sources section genuinely good: a clear add-source entry point (local
folder now, remote "coming soon"), richer source management, **and** an optional
**managed library** that organizes files into our structure — built as a layer
that *feeds* the read-only core rather than fighting it.

## The core architectural decision (the heart of this spec)

Separate two layers that the original blurred:

- **Read path** — index + decode + playback. This is `soul-audio-core`: `no_std`,
  frozen, **never mutates files**. The DAP *playback runtime* is this. Unchanged.
- **Write path** — copy / move / organize files into a folder structure. This is
  import/transfer. It is a **separate `std`-side layer** that *produces* a clean
  on-disk tree; the read path then scans and reads it. The two **compose**; they
  do not conflict.

**A managed library is DAP-supporting, not DAP-hostile.** A DAP's storage (SD /
internal) is the curated destination, so "organize music onto the device" is the
canonical way music lands on a DAP — arguably more central there than on desktop.
The *same* Rust organize logic serves desktop "import into my library" and a DAP
management mode's "transfer to device, organized." The one rule that keeps this
clean: **the organize logic lives in a new `std` crate beside the core, never
inside the `no_std` index crate.** The DAP playback runtime never links it; the
DAP's management mode and the desktop app do.

Consequence: we keep **both** source modes, side by side (this is the original's
real model):

- **Watched folder** (reference): scanned in place, never touched. What we have.
- **Managed library** (write): a destination folder we organize *into* (copy or
  move, via a path template) — and that destination is then itself just a watched
  folder. **No separate database**: the catalog/index stays the source of truth
  for "what's in the library," so the organizer can be **stateless** (read tags →
  compute destination path → copy/move → let the normal scan index it).

---

## Phase 1 — Reference-side Sources revamp (ships first; `-ng` only)

Fast, safe, visible, no file mutation. Delivers the "handle sources much better"
ask and the "Directory + Remote (coming soon)" add-source entry point.

### 1.1 Add-source entry point

Replace the single `+ Add Source` button with a themed `views::dropdown` menu
(the arch test bans native `PopupMenu::picker`, so reuse `views::dropdown`),
anchored under the button, with two items:

- **"Local folder…"** (enabled) → the existing `tx.open_folder(..)` →
  `host.add_folder` flow, unchanged.
- **"Remote server… (Soon)"** (disabled/greyed, with a "Soon" chip) → no action.
  Mirrors the original, which also shipped this as Coming Soon.

### 1.2 Richer source rows

Each row shows:

- **Name** (basename) as the title, **full path** as a truncating secondary line
  (today only the basename is shown).
- **Track count** contributed by that source (e.g. "1,234 tracks"), via i18n
  pluralization.
- **Offline** badge when unavailable (kept).
- An **enable/disable** toggle at the row start. A disabled source stays listed
  and configured but contributes no tracks and is skipped on scan (greyed).
- **✕ remove** (kept).

### 1.3 Host contract change (soul-player-host)

Track count and enable/disable are not derivable from today's `WatchUpdate`
(`TrackView` carries no source id), so the contract gains per-source status:

```rust
pub struct SourceStatus {
    pub path: PathBuf,
    pub enabled: bool,
    pub available: bool,    // reachable on the last scan
    pub track_count: usize, // tracks this root contributed (0 if disabled/unavailable)
}

pub struct WatchUpdate {
    pub views: Vec<TrackView>,
    pub sources: Vec<SourceStatus>, // REPLACES `unavailable`
}
```

`unavailable` becomes `sources.iter().filter(|s| !s.available)` — derived, not
stored. Internal `folders: Mutex<Vec<PathBuf>>` becomes an ordered
`Vec<FolderConfig { path, enabled }>`. New/changed methods:

- `add_folder(dir)` — adds **enabled** by default (returns `WatchUpdate`).
- `remove_folder(dir)` — unchanged semantics.
- `set_folder_enabled(dir, enabled) -> WatchUpdate` — new; flips a source,
  re-aggregates (disabled roots skipped, still listed).
- `sources() -> Vec<SourceStatus>` — replaces `watched_folders() -> Vec<PathBuf>`
  as the status read (a thin `watched_folders()` may stay as a path-only
  convenience if callers need it).

Aggregation counts tracks per **available + enabled** root while it already
iterates roots to classify available/unavailable — cheap, no extra scan pass.

### 1.4 Config change (`-ng` config.toml)

Keep `sources: Vec<String>` (paths, order preserved) and **add**
`sources_disabled: Vec<String>` (the subset currently disabled; `#[serde(default)]`
→ empty). Old configs load unchanged (everything enabled). This avoids fragile
string→table migration in a TOML array and is free to roll back.

### 1.5 `-ng` state + UI wiring

`State` replaces `watched_folders + unavailable` with
`sources: Vec<SourceStatus>` (or keeps `watched_folders` for ordering and adds a
`source_status` map — implementer's call during planning). `apply_watch` /
`apply_and_save` fold the new `WatchUpdate.sources` in and persist enabled state
to `sources_disabled`.

### 1.6 i18n

New keys in `en.yaml` (mirrored to de/es/zh/cs; English acceptable as a
placeholder pending translation): `settings.addLocalFolder`,
`settings.addRemoteServer`, `settings.comingSoon` (reuse `common.comingSoon` if it
exists), `settings.trackCount` (plural), `settings.enableSource` /
`settings.disableSource`, `settings.sourceDisabledHint`. Single-quote YAML values
per house style.

### 1.7 Tests (Phase 1)

- **Host unit:** a disabled folder contributes 0 tracks and stays in `sources`;
  `set_folder_enabled` re-aggregates; `SourceStatus.track_count` sums correctly
  across multiple roots; `available=false` for a missing root (self-heal kept).
- **Rune headless (`Runtime`):** add-source menu opens and contains an enabled
  "Local folder" item and a disabled "Remote server (Soon)" item; a source row
  renders full path + track count; toggling the enable control flips the source
  and the library re-aggregates.
- Cross-locale overflow test (reuse the existing `rt.elements()` harness) for the
  new row layout.

---

## Phase 2 — Managed library / organize crate (own detailed plan later)

Bigger and riskier (it moves/renames the user's files), so it lands as a
self-contained, well-tested effort after Phase 1. Design resolution below; a
dedicated implementation plan (and possibly its own sub-spec) comes when we start
it.

### 2.1 New `std` crate beside the core

A new crate in the **soul-audio-core** repo, e.g. `soul-audio-organize`
(name is a decision — see below), `std`, pure logic, **no UI, no database**. It
depends on `soul-audio-tags` (already in core) to read tags. It is **not** a
dependency of the `no_std` index/playback crates, so the DAP playback runtime
never links it. Landing it means a core change → bump core across `-ng`'s three
lockfiles (`cargo update --precise <sha>` in root/service, app/rune, host).

### 2.2 Stateless plan → apply API

```rust
pub enum Strategy { Copy, Move }           // Copy is the default
pub struct Template(String);               // "{AlbumArtist}/{Album}/{TrackNo} - {Title}"
pub enum Action { Copy, Move, SkipDuplicate, SkipConflict }
pub struct PlanEntry { pub src: PathBuf, pub dst: PathBuf, pub action: Action }
pub struct OrganizePlan { pub entries: Vec<PlanEntry> }

/// Reads tags only — NO filesystem mutation. Produces a previewable plan.
pub fn plan(files: &[PathBuf], dest_root: &Path, template: &Template, strategy: Strategy) -> OrganizePlan;

/// The only mutating call. Idempotent: re-running skips already-present files.
pub fn apply(plan: &OrganizePlan, on_progress: impl FnMut(Progress)) -> std::io::Result<Summary>;
```

- **Template engine:** tokens `{AlbumArtist} {Artist} {Album} {Year} {Disc}
  {TrackNo} {Title} {Genre}`, `/` = path separator, filesystem-illegal chars
  sanitized, zero-padded track numbers. Presets + a custom field + **live
  preview** rendered from a sample track.
- **Dedup:** skip when a file already exists at the computed destination, and
  content-hash within the batch to skip identical inputs. Stateless — no DB.
- **Conflicts:** destination exists but differs → `SkipConflict` (surfaced in the
  preview; never silently overwrite).

### 2.3 Safety (non-negotiable — we touch user files)

- `plan()` is read-only; the UI shows the full plan as a **preview/dry-run**
  before anything happens.
- **Copy is the default**; **Move** requires explicit confirmation.
- Never destructive without confirmation; **idempotent** re-runs.

### 2.4 `-ng` UI

A third add-source item **"Managed library…"** (or a dedicated card): pick a
**destination folder** + a **template** (presets / custom + live preview) +
**copy|move**. An **"Add music…"** action (files or a folder) runs `plan()`,
shows the preview, then `apply()` with a progress UI (imported / skipped /
failed), mirroring the original's import dialog. The destination auto-registers
as a watched source, so the organized tree indexes through the normal scan path.

### 2.5 DAP reuse

Because the crate is pure `std` logic with no UI/DB, a DAP's management/companion
mode links it to lay out tracks on the card; the DAP playback runtime (the
`no_std` core) just scans the result. Same code, two heads — the project's
"shared logic in core" principle, applied to the write path.

---

## Phase 3 — Remote server sources (future; stub only now)

The original's Soul Server stack (URL + test-connection + auth + sync) is a large
future effort. For now the add-source menu's "Remote server…" item stays a
disabled **"Soon"** entry. Not designed here.

---

## Decisions & assumptions (please veto any during review)

1. **Reference stays the default; managed library is additive.** We are **not**
   building the original's SQLite-backed managed library — the catalog/index is
   the source of truth, so the organizer is stateless.
2. **Organize logic → new `std` crate in the core repo** (not in-app, not in the
   `no_std` core). Proposed name **`soul-audio-organize`** (open to `soul-importer`
   or another).
3. **Phase 1 includes enable/disable + track count**, which require the
   `WatchUpdate`/host contract change in §1.3. If you'd rather keep Phase 1 to
   just the add-source menu + full-path display (no host change), we drop §1.3/§1.2
   toggles to a follow-up.
4. **No "reference" import strategy** (the original had copy/move/reference):
   "reference" in our model is simply *using a watched folder* — so managed
   library offers only Copy/Move.
5. **Config migration is additive** (`sources_disabled`), not a schema rewrite.
6. **i18n:** new keys land in all five locales with English as the stopgap for
   non-en until translated.

## Out of scope / YAGNI

- Sync-deletes toggle, MusicBrainz confidence matching, "require review" bucket,
  external-file "ask/play/import" routing (original features we don't need yet).
- Remote server sources (Phase 3 stub only).
- Tag **editing** / write-back (separate effort; unrelated to organizing files).

## Open questions

- Crate name for the organize layer (§Decisions 2).
- Phase 1 scope: include the host contract change for enable/disable + counts, or
  defer it (§Decisions 3)?
