# Loudness / Dynamic-Range Library Column — Design

**Date:** 2026-11-21
**Status:** Design (approved direction; drives the implementation plan)

## Goal

Show per-track **Integrated LUFS** and **DR** (dynamic range) in the library list, sortable, so a
listener can find quiet/loud tracks and spot loudness-war victims. The metrics are produced **during
the library scan** (a second, deferred analysis tier) rather than only lazily on play, and persisted
so analysis is one-time.

## Why this shape (research-grounded)

Every established library separates a **fast metadata tier** (tag read) from a **slow loudness tier**
(full audio decode). foobar2000 is manual/on-demand (right-click → scan, results in tags);
Roon is automatic/background (analyze the whole library on import, plus on-demand on play), governed
by a **"Background analysis speed" setting defaulting to "Throttled"**, non-invasive DB storage.
Standalone scanners (rsgain, r128gain, foo_truepeak) are the manual model unbundled; foo_truepeak
gets RG + LRA + DR in **one decode pass**.

Community forums add three concrete requirements:
1. **Playback dropouts from background analysis are the #1 complaint** (Roon). The deep tier MUST yield
   to active playback, not merely to general CPU load.
2. **A real "Off"** matters — the bit-perfect/minimalist crowd wants zero background CPU while
   listening.
3. **DR is contested.** The audiophile "DR" (TT/Pleasurize, the dr.loudness-war DR14 database) is
   **crest factor, not true dynamic range**; a lone transient inflates it, vinyl/genre skew it, higher
   is not always better. The one durable signal: **DR ≤ ~8 = compressed**. So we present DR as an
   *indicator* (color-banded), labeled "DR" (not "Dynamic Range"), not a quality score. (Roon's "DR" is
   actually R128 LRA — a different number; we keep LRA/PLR/true-peak in the detail panel.)

We adopt the **Roon model**, which fits because we already own both halves: the adaptive index-speed
knob (CPU-idle `LoadSampler`) is a *better* throttle than Roon's fixed single-core, and lazy-on-play
already exists as the on-demand fast-path. Storage stays **non-invasive/derived** (our `.soulidx` +
cache), never writing tags (a possible separate opt-in later).

## What already exists (reuse, do not rebuild)

- Core `soul-audio-loudness::R128Analyzer` — streaming `feed()`/`metrics()`, O(1) memory, no_std,
  ffmpeg-cross-checked. `TrackLoudness { integrated_lufs, lra, true_peak_dbtp, plr, dr }` (5 f32).
- `soul-player-backend/src/loudness.rs` — a throttled worker (64 MiB stack), single-flight, stamp-cached,
  event-emitting; `analyze_track(path)` full-decodes → `R128Analyzer`. Today fired only for the playing
  untagged track under ReplayGain.
- Path-keyed persistence: in-memory `BTreeMap` + `loudness_cache.txt` sidecar (5 metrics + mtime + size +
  path; freshness by mtime+size) + `SeedLoudness` restore on startup.
- Scan pipeline: `scan_incremental` (tag read only) + mtime+size `FileStamp` incremental skip + the
  adaptive knob (`IndexingMode` + `LoadSampler` + a live `AtomicUsize` target) + `ScanProgress`
  {phase, done, total} → titlebar widget.
- **The reserved `.soulidx` seam**: `TrackRec.flags: u16` ("has analysis" bit) + `TrackRec.extended_off:
  u32` into a never-yet-written `extended.blob`, documented as the home for "Tier-2/Tier-3 per-track
  payloads." Loudness is its first consumer; fingerprint/MBID (the 2026-11-14 roadmap) reuse it later.

## Architecture — a second scan tier

**Tier 1 (unchanged):** metadata scan → bake `.soulidx` → library usable immediately.

**Tier 2 (new): background loudness/DR analysis**, kicked off by the scan:
- Reuses the loudness worker + `R128Analyzer`, fed the scanned/changed tracks (not the playing one).
- **Incremental** via the same mtime+size stamp (analyze once ever; skip unchanged; the `None` sentinel
  already prevents retrying un-analyzable tracks — DSD/>2ch).
- **Throttled** by a dedicated setting (below), and **yields to active playback** (the key forum
  lesson): while a track is playing, the sweep pauses or drops to minimal concurrency, independent of
  CPU-idle, so it can never cause dropouts. The playing track still gets on-demand priority (unchanged).
- **Cancellable** via an `AtomicBool` stop flag (the scan has none today; model on `LoadSampler`'s
  stop-and-join), and on a new scan the in-flight sweep is superseded.
- **Progress**: a new `ScanPhase::Analyzing` reported through `ScanProgress` → the titlebar widget
  ("Analyzing N/M"), promoting the callback toward the roadmap's `Event::ScanProgress` (A5).

## Data model & storage

- **TrackLoudness** is the payload (already defined). The library needs `integrated_lufs` + `dr`; the
  full set stays available for the detail panel.
- **`.soulidx` seam (the designed path):** a new `extended.blob` section holding per-track TLV records;
  `TrackRec.flags` gets a `HAS_LOUDNESS` bit and `extended_off` points at the track's TLV. A `Loudness`
  TLV type carries the 5 f32. Format bumps `FORMAT_VERSION` (migration-free; rebaked each scan). The
  bake accepts optional per-track loudness (from the cache) and emits the TLV when present.
- **Cache (the live source):** the path-keyed, stamp-validated cache is what the Tier-2 pass writes and
  what the live UI reads. It updates per analysis event, so the column fills **progressively without
  re-baking**. This is the authoritative in-session source.
- **Seam (the baked/DAP representation):** on a (re)bake the cache populates the `extended.blob` seam, so
  the `.soulidx` carries loudness for the **DAP** and a unified index. It lags Tier-2 (only refreshes on a
  bake) and is NOT the live UI path. For warm startup the cache is seeded and the bake stamps the seam
  with no re-analysis.
- **Projection:** `TrackView` gains `loudness: Option<TrackLoudness>`, filled by a **host-side join of the
  live cache** onto the `views_by` output (exactly how `mark_remote` post-processes views today), keyed
  by the track's path. This gives progressive updates with no re-bake churn. (The baked seam carries the
  same data in the index for the DAP; it is not read back for the live `-ng` column.)

## Settings

A dedicated **"Background loudness analysis"** control (Settings → Library), independent of the
metadata index-speed knob, mirroring Roon's "Background analysis speed":
- **Off** — no background sweep; lazy-on-play analysis still fills the playing track (and its column).
- **Throttled (default)** — low-priority sweep, CPU-idle-aware + playback-yielding.
- **Full** — use the adaptive pool up to the ceiling to finish fast (for "just get it done").

## UI

- Two right-aligned columns in each library row, mirroring the existing `duration` column
  (fixed width, secondary color): **DR** (headline, color-banded like the signal-path panel — red ≤8,
  warm mid, green high) then **LUFS** (`-14.0`). Un-analyzed → a muted "·" / "—".
- **Compact mode** (pane ≤ 640): show **DR only**, hide LUFS (as artist/album already hide).
- **Sort**: two new sort pills, "DR" and "Loudness". Loudness is not a baked `BrowseAxis`, so these do a
  **client-side runtime sort** of the current view (`Vec<TrackView>` by the joined metric); un-analyzed
  tracks sort last (stable). Selecting a loudness sort also ensures the current view's un-analyzed
  tracks are enqueued (so the sort becomes meaningful as results arrive), respecting the setting.
- Titlebar widget shows the `Analyzing N/M` phase during Tier 2.

## Testing strategy (compare against other solutions; TDD)

The whole point of loudness is numeric correctness, so the backbone is **conformance against
independent oracles**, exactly like the downmix work used ffmpeg.

1. **Metric conformance (core, the oracle comparison).** The `R128Analyzer` is already ffmpeg-cross-checked
   for LUFS/true-peak. Extend/assert: (a) Integrated LUFS vs **ffmpeg `ebur128`** within ≤0.1 LU on
   committed fixtures (the existing bar); (b) **DR** vs an independent TT-DR reference value (precomputed
   with `dr14_t.meter`/`foo_dr_meter` and committed as the golden, since our `dr` is the TT crest-factor
   definition) within ±1 DR; (c) LRA vs ffmpeg `ebur128` LRA. These make "compare with other solutions"
   a committed, CI-running assertion, not a manual check.
2. **Seam round-trip (core).** Bake a catalog with per-track loudness → read via `IndexReader` → the TLV
   decodes bit-identical; the `flags` bit + `extended_off` are set iff loudness present; un-analyzed
   tracks carry no TLV and read back `None`. Format-version bump verified (old 4-field cache ignored).
3. **Tier-2 orchestration (-ng).** A temp library of a few known fixtures: run a scan → the sweep
   analyzes each, the cache + seam populate, `TrackView.loudness` is `Some`; a second scan **skips**
   (stamp unchanged, no re-decode — assert via a decode counter/log); a changed file re-analyzes;
   **cancel** stops promptly; **playback-yield** — with a track "playing," the sweep's effective
   concurrency drops (assert the throttle state), proving the dropout guard.
4. **End-to-end (-ng).** Drive the real projection: scan a fixture folder → `host.views_by` returns
   `TrackView`s whose `loudness` matches the per-fixture golden; the client-side **DR/Loudness sort**
   orders them correctly with un-analyzed last. This is the library-column analogue of `signal_e2e`.
5. **Fixtures.** Reuse/extend the committed loudness fixtures; pick 3-4 tracks spanning a loud/compressed
   (low DR) and a dynamic (high DR) case so the sort + color-band are exercised with known goldens.

Every task is TDD: write the failing conformance/round-trip/e2e test first, watch it fail, implement.

## Increments (sequencing)

- **PR 1 (core):** the `extended.blob` TLV infrastructure + a `Loudness` TLV + `TrackRec` `flags`/
  `extended_off` wiring + `IndexReader` exposes loudness + bake accepts per-track loudness + format bump.
  Tests: seam round-trip + the metric-conformance goldens (DR/LUFS/LRA vs oracles).
- **PR 2 (-ng):** the scan-driven Tier-2 pass (reuse the worker; incremental stamp; throttle + the
  playback-yield + cancel; `ScanPhase::Analyzing` progress) + feed loudness into the bake + the cache
  wiring + bump core. Tests: orchestration (analyze/skip/re-analyze/cancel/yield) + the seam carries it.
- **PR 3 (-ng):** the UI — `TrackView.loudness` projection + DR + LUFS columns (color-band, compact) +
  client-side DR/Loudness sort + the setting (Off/Throttled/Full) + titlebar phase. Tests: the e2e
  projection+sort + a layout/compact test.

## Global constraints

- No em-dashes in UI copy, commits, or prose (commas/colons/parens).
- Conventional Commits; the commit-msg hook rejects any Claude/Anthropic/AI/co-author reference; use
  `git commit -F -` heredocs.
- Core change in an isolated worktree; `-ng` is THREE workspaces (root/service, app/rune nightly,
  crates/soul-player-host) — a core bump must `cargo update --precise <sha>` all three.
- Core pre-push runs the FULL gate (incl. `embedded-profile` + thumbv7em); feature-gate anything that
  would bloat the embedded SRAM budget. The `extended.blob`/TLV must cross-build no_std.
- Verify locally; merge `-ng` PRs `--squash --delete-branch` after CI green (rust service+host legs are
  the ones that must pass; rune legs may show the known i18n locale flake).

## Non-goals / deferred

- **Album-level DR/loudness** (Roon shows it; foobar scans "as albums"). Track-level first; album
  aggregation is a clean follow-up.
- **Writing ReplayGain/loudness tags** to the files (foobar's model) — stays non-invasive; a separate
  opt-in export later if wanted.
- **Live loudness meter** (the other deferred item from the loudness program) — unrelated.
- Reusing the `extended.blob` seam for fingerprint/MBID — that's the 2026-11-14 roadmap; this spec only
  builds the seam and is its first consumer.
