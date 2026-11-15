# Cloud Enrichment Architecture — client ↔ service contract

**Status:** Design · **Date:** 2026-11-15 · **Scope:** the split between the open-source Rust **client** (soul-player-ng + the future DAP) and a **separate, closed-source cloud service** (Go or .NET) that does identity-matching + metadata enrichment.

> This doc is the contract the cloud team builds against. The **client half is implemented** (`soul-player-backend::fingerprint` + the tags-MBID baked into `.soulidx` v4). The **cloud is intentionally out of this repo** — a different language, closed source, its own lifecycle.

## North star

The client computes a track's **identity** and asks our cloud, "what is this, and what do you know about it?" The cloud owns the hard parts — a fingerprint→recording **matching index** and an aggregated **metadata graph** (MusicBrainz + Discogs + Bandcamp) — and runs *our* algorithms on top. The client never calls AcoustID/MusicBrainz directly; it only talks to us.

## Why this split

- **Privacy:** only a one-way fingerprint + any tags-MBID leaves the machine — never audio. Opt-in.
- **Leverage:** the cloud is where the data, the index, and the proprietary algorithms live. Swapping a metadata source (e.g. dropping Bandcamp for ToS reasons) never touches shipped clients.
- **Moat:** every user's *tagged* file is a free `(fingerprint, MBID)` pair. Opt-in submissions grow *our* index so untagged matching improves over time — exactly how AcoustID bootstrapped.

## Identity: Chromaprint is the lingua franca

The client fingerprints the **clean original files** it already decodes — near-identical matching, Chromaprint's design point (compact ~2.5 KB, fast). Neural fingerprinting's robustness to noise/reverb/mic is irrelevant here and needs a model; **do not** use it.

Chromaprint is the one format that exists natively in every language we care about, and its **matching is bit-error-tolerant** (align + BER), so a resampler difference between client and cloud doesn't break a match — you do **not** need bit-identical fingerprints, only the same algorithm (**default Algorithm 2**), 11025 Hz target, and frame structure.

| Side | Lib | License note |
|---|---|---|
| **Rust client** | `rusty-chromaprint` (Algorithm 2, `preset_test2`) | **MIT** (rubato resampler) — a *distributed* client must stay permissive. `chromaprint-next` is faster/bit-identical but its resampler is LGPL-2.1. |
| **Go cloud** | `gochromaprint` | pure Go, no CGO |
| **.NET cloud** | `AcoustID.NET` | C# Chromaprint port |

> `SoundFingerprinting` (.NET) is a great library but a **different, .NET-only** algorithm — the Rust client can't produce it, so it's off the table for this design.

## The client → cloud contract

The client submits a batch of identities (opt-in, cached, offline-degrading). Per track:

```jsonc
// POST /v1/identify   (batched)
{
  "tracks": [
    {
      "client_ref": "opaque-local-id",      // client correlates the response; not stored by us
      "fingerprint": "AQAAR0mUa...",         // Chromaprint, compressed + URL-safe base64 (no pad)
      "duration_secs": 213.6,                // AcoustID-style; used to align the match
      "mbid": "d6d5e8e6-…"                   // OPTIONAL recording MBID read from tags; skip matching when present
    }
  ]
}
```

```jsonc
// 200 OK
{
  "results": [
    {
      "client_ref": "opaque-local-id",
      "recording_mbid": "d6d5e8e6-…",        // resolved identity (from tags or the match)
      "confidence": 0.98,                    // 0..1 — match score (1.0 for a tags-MBID hit)
      "metadata": {                          // the enrichment, canonicalized across sources
        "title": "…", "artist": "…", "album": "…",
        "genres": ["…"], "year": 1959,
        "release_mbid": "…", "artist_mbids": ["…"]
      }
    }
  ]
}
```

- The compressed-base64 form is exactly what `Fingerprint::compressed_base64()` emits and what `fpcalc`/AcoustID transmit — the cloud (or AcoustID itself) decodes it directly.
- **Tagged shortcut:** when `mbid` is present, skip matching entirely — look it up in the graph. Matching is only for untagged files.
- Rate-limit + batch; cache responses by fingerprint so a re-scan doesn't re-query.

## The client → cloud **submission** (the flywheel)

For **tagged** tracks (fingerprint *and* MBID known), the client may submit the pair so the cloud grows its own index:

```jsonc
// POST /v1/submit   (opt-in, separate consent from identify)
{ "pairs": [ { "fingerprint": "AQAA…", "duration_secs": 213.6, "mbid": "d6d5e8e6-…" } ] }
```

This is how the cloud's `fingerprint → recording` index grows without needing anyone's audio — and reduces dependence on AcoustID's DB over time.

## What the cloud must build

1. **Matching index.** Start simple: **PostgreSQL + a GIN index** on each fingerprint's sub-hash array (AcoustID ships a PG extension doing exactly this — query takes the middle of the fingerprint, strips to 28 bits, array-overlap → candidates, then BER-rank in app code). Scale path: a dedicated inverted index like AcoustID's **`acoustid-index`** (rewritten in Zig — 100M fingerprints, ~10 ms/search) called as a sidecar. **No custom index server is needed to launch.**
2. **Aggregated metadata graph**, keyed by recording MBID + fingerprint cluster:
   - **MusicBrainz** — CC0, safe to host/aggregate. Bulk via the CC0 PostgreSQL dumps (no rate limit); live WS/2 `lookup?inc=genres+tags` only as a fallback (≤1 req/s + descriptive User-Agent).
   - **Discogs** — official API (OAuth, rate-limited) + a monthly dump under a data license; prefer those over scraping; review redistribution terms.
   - **Bandcamp** — ⚠️ no public API, ToS restricts scraping → **highest legal risk**. Build the graph **source-agnostic** so Bandcamp can be swapped/removed for compliance without touching the algorithms. Consider official/partner channels or user-initiated per-item fetches; **legal review before ingesting.**
3. **Our algorithms** on top: canonical reconciliation (one artist/album/genre across sources), similarity/recommendations, duplicate/edition detection, enrichment confidence scoring.
4. **Crowdsource ingest** (`/v1/submit`) → the index-growth flywheel.

## Licensing — why a closed service is fine

- **LGPL (Chromaprint C / chromaprint-next):** triggers on *distribution*; a hosted service distributes nothing, so the cloud may use any variant server-side. Only the **shipped Rust client** must stay permissive → `rusty-chromaprint` (MIT).
- **AcoustID DB (CC BY-SA 3.0):** in a *service* context share-alike reaches only the distributed **output**, not the internal DB — and CC BY-SA **3.0 removed database rights from scope**. So a closed cloud can use the **dump** (not the live API) commercially with **attribution** (credit AcoustID/MusicBrainz). *Get a one-time legal read before launch.*
- **MusicBrainz:** CC0 — unrestricted.

## Go vs .NET

The workload has two halves and they pull slightly differently:
- **Match service** (HTTP/gRPC + Postgres + popcnt/BER over candidates + ingest): systems-y, high-concurrency, lean → **Go** fits best (goroutine fan-out, tiny binaries, `pgx`, pure-Go Chromaprint, mirrors AcoustID's own architecture niche).
- **Enrichment/algorithms** (MB/Discogs/Bandcamp ETL, reconciliation, scoring): business-logic-heavy → **.NET** shines (EF Core, rich libraries, strong SIMD intrinsics).

**Recommendation:** one language → **Go** (the identity/matching path is the hot, scale-sensitive part and Go is the cleaner fit). Choose **.NET** if C# is the team's strength or the aggregation/algorithms layer dominates. A later split (Go match-service + .NET enrichment-service over gRPC) is viable — don't start there.

## Degradation & privacy
- Offline / cloud-unreachable → tags-only metadata; cache enrichment client-side aggressively.
- Both `identify` and `submit` are **opt-in**, with separate consent. Never send audio. The fingerprint is one-way (not reversible to audio).

## Status / sequencing
- ✅ **Client identity shipped:** tags-MBID baked into `.soulidx` v4 (core #74/#75) + `soul-player-backend::fingerprint` (`fingerprint_file` / `fingerprint_pcm` / `Fingerprint::compressed_base64`).
- ⏭️ **Client next:** a background "identify" job (reuse the loudness-worker pattern — a 64 MiB-stack worker) that fingerprints the library, caches results by file-stamp, and (opt-in) calls `/v1/identify` + `/v1/submit`; surface "N identified". Gated on the cloud existing + the opt-in UX.
- ⏭️ **Cloud:** the separate Go/.NET project per this contract.

## Sources
- [Chromaprint](https://github.com/acoustid/chromaprint) · [Inside the AcoustID server (PG + GIN)](https://oxygene.sk/2011/12/inside-the-acoustid-server/) · [acoustid-index (Zig)](https://deepwiki.com/acoustid/acoustid-index) · [AcoustID license](https://acoustid.org/license) · [CC BY-SA & database rights in services](https://www.scl.org/2053-creative-commons-licences-what-to-do-with-the-database-right/) · [rusty-chromaprint (MIT)](https://lib.rs/crates/rusty-chromaprint) · [gochromaprint](https://github.com/alexgorbatchev/gochromaprint) · [SoundFingerprinting (.NET)](https://github.com/AddictedCS/soundfingerprinting)
