# Metadata & Indexing Program — Plan

**Status:** Plan for review · **Date:** 2026-11-14 · **Repos:** soul-audio-core + soul-player-ng (+ a future cloud service)

## North star
Best-in-class local scanning **and** authoritative metadata — a scanner that's fast, correct, and live; identity via acoustic **fingerprint + MusicBrainz ID**; and a future **cloud enrichment service** (MusicBrainz + Discogs + Bandcamp) that powers *our own* algorithms (recommendations, similarity, dedup, canonical metadata). The fingerprint/MBID is the key that ties local files to all of it.

## Where we are (shipped)
- **`scan_incremental`** (core #68): incremental stamp-diff cache (skip unchanged) + parallel extraction (`std::thread::scope`) + deletions + progress hook; byte-identical bake; benched ~4× cold-vs-warm.
- **Genre** indexing (`.soulidx` v3, #67/#140) and the **managed-library** organizer (#63/#136).
- Honest grade of the scanner today: **A− foundation**, with two standouts (the DAP-targeted frozen index; change detection that catches in-place edits Navidrome's folder-hash misses) and clear gaps: no persistence/warm-startup, discovery stats every file (no folder-level skip), no live watcher, no fingerprint identity, no network-share throttling.

This program closes those gaps and builds the identity + enrichment layers.

---

## Phase A — Finish the scanner → genuinely best-in-class
Near-term. Core = the algorithm; -ng = integration + persistence + UI. Each a small PR.

- **A1 — `-ng` integration.** `soul-player-backend::Library` calls `scan_incremental` per source (today it full-rebakes every root). Hold a `ScanCache` per source in the host. TDD: a warm rescan re-extracts nothing; add/change/remove reflected.
- **A2 — Persistence (warm startup).** Persist each source's `ScanCache` to disk (and optionally the baked `.soulidx`) so a launch is a warm diff, not a cold full read. Core: make `ScanCache`/`RawTrack` serializable (serde behind a feature, OR a compact bespoke format) + a version tag (invalidate on importer-format change). `-ng`: store under the app config/cache dir, keyed by source path. **This is the single biggest real-world win** — today every launch cold-reads the whole library.
- **A3 — Folder-mtime fast-path (the efficiency closer).** Before stat-ing every file, compare each **directory's** mtime (and a cheap folder signature: sorted child names + count) to the cached folder signature; **skip unchanged directories wholesale** (like Navidrome's quick scan) — but *within* a changed directory keep the per-file `(mtime,size)` stamp, so we DON'T inherit Navidrome's documented bug of missing in-place edits. Best of both: O(changed folders) discovery + per-file correctness. Core change to the discovery walk + the cache (store folder signatures).
- **A4 — Deletion warnings.** `ScanOutcome.removed` → a host `Event` → a UI toast ("N tracks removed from <folder>"). Distinguish a per-file delete (available root) from a whole offline drive (existing root-unavailable path — never mass-report).
- **A5 — Progress event + bar.** Adapt the `ScanProgress` callback into a host `Event::ScanProgress` (same fan-out as `Tick`/loudness). Then a small progress UI (indeterminate during Discovering, determinate during Extracting).
- **A6 — Live watcher.** A debounced `notify`-crate watcher on each watched root → incremental rescan on change while the app is open. `-ng` only (a DAP reads a static card). Debounce bursts (an album copy = hundreds of events).
- **A7 — Robustness / throttling.** Make the extraction thread count a config knob; default oversubscribed for local disks but **bounded/low for network shares** (unbounded parallelism is wrong for a NAS). Interruption-safe (partial cache still valid — it's keyed per file).
- **A8 — (optional) content-hash verify.** For paranoid correctness (a same-size, same-mtime edit — rare), an optional per-file content hash on suspect files. Off by default (cost).

Exit criterion: warm startup is near-instant on a large static library; edits/adds/deletes reflect live; discovery is O(changed folders); honestly **S+** for the desktop-bakes-for-DAP shape.

---

## Phase B — Acoustic fingerprinting + MusicBrainz identity
The identity layer. This is where "proper fingerprinting" lands.

- **B1 — Chromaprint fingerprint in core.** A new `std` crate `soul-audio-fingerprint` (StdAdapter): decode a file's PCM via our existing `soul-audio-core` decoder → feed a **pure-Rust Chromaprint** (`chromaprint-next` — bit-identical to the C reference, ~4% faster, no C FFI; verify its license clears cargo-deny/Apache-2.0) → `(fingerprint, duration_secs)`. We already decode audio, so this is additive and FFI-free. Compute it lazily (it needs a full decode — heavier than a tag read), off the critical path, cached by file stamp like `RawTrack`.
- **B2 — MBID resolution (two tiers).**
  1. **From tags first** — add `MUSICBRAINZ_TRACKID` / recording-id parsing to `soul-audio-tags` (`Field::MusicBrainzRecordingId`: Vorbis `MUSICBRAINZ_TRACKID`, ID3 `TXXX:MusicBrainz Recording Id`/`UFID`, MP4 `----:…:MusicBrainz Track Id`). Picard-tagged libraries resolve instantly, zero network.
  2. **AcoustID fallback** — fingerprint+duration → the AcoustID API (needs a free API key + descriptive User-Agent; ≤3 req/s) → recording MBID(s). For untagged files.
- **B3 — Catalog carries the MBID.** Add a recording-MBID field in a `.soulidx` format bump (migration-free — the index is rebaked each scan, so no cost; see [[musicbrainz-enrichment-plan]]). This becomes the enrichment key.
- **B4 — The enrichment seam.** `import::scan` already has the spot: between read-tags and intern, augment metadata by MBID (fill/override genre, artist, etc.). Online (rate-limited MB `lookup?inc=genres+tags`) or offline (CC0 MB Postgres dumps via a local mirror). Keep it a pluggable step so the cloud service (Phase C) can back it.
- **B5 — UX.** A background "identify/enrich" job (reuse the loudness-analyze pattern) + an "N tracks identified" surface; never block playback.

---

## Phase C — Cloud enrichment service ("our own algorithms")
Far-term, a new backend. Design intent only here.

- **C1 — Aggregate graph.** A service that unifies **MusicBrainz** (CC0 — free to use/host) + **Discogs** + **Bandcamp** into one metadata graph keyed by recording/release MBID and acoustic fingerprint. Local files map in via the Phase-B fingerprint/MBID.
- **C2 — Our algorithms.** On top of the graph: recommendations, similarity, cross-source reconciliation (canonical artist/album/genre), duplicate/edition detection, enrichment confidence scoring.
- **C3 — Client integration.** The app/DAP queries the service (or a synced local mirror) for enrichment; the scanner's fingerprint/MBID is the join key. Cache aggressively client-side; degrade to tags-only offline.
- **C4 — ⚠️ Legal / ToS (flagging honestly).** **MusicBrainz is CC0** — safe to aggregate and host. **Discogs** has an official **API** (OAuth, rate-limited) and a **monthly data dump under a data license** — prefer those over scraping; review the license for redistribution terms. **Bandcamp has no public API and its ToS restricts scraping** — bulk scraping is legally/contractually risky and brittle; treat it as the highest-risk source (consider official/partner channels, user-initiated per-item fetches, or dropping it) before building on it. Build the service **source-agnostic** so a source can be swapped/removed for compliance without touching the algorithms. Get legal review before shipping Discogs/Bandcamp ingestion.

---

## Sequencing & dependencies
1. **Phase A first** (A1→A2→A3 are the highest-value: integration, warm startup, folder-skip). A4–A8 follow.
2. **Phase B** builds on A (the enrichment job rides the same scan/background infra; B3's MBID field is a free format bump).
3. **Phase C** is a separate backend; it consumes B's fingerprint/MBID. Gate on legal review (C4).

## Open design calls (confirm before building)
1. **Cache persistence format** — serde on `ScanCache` (simple, a dep) vs a compact bespoke binary (lean, more work). Lean toward serde behind a feature for v1.
2. **Fingerprint crate** — `chromaprint-next` (bit-identical, faster) vs `rusty-chromaprint`; verify license + maintenance before picking.
3. **Enrichment default** — opt-in (privacy: AcoustID/MB calls leave the machine) or on-by-default with a toggle? Lean opt-in with a clear switch.
4. **AcoustID key** — ship our app's key, or ask the user for one? (App key is standard for open-source players.)
