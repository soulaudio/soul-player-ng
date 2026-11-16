# Loudness & Dynamic-Range Metrics (#110) — Design

**Status:** design (scope approved: per-track scanned numbers; live meter deferred)
**Area:** `soul-audio-core/soul-audio-loudness` (DSP) + `soul-player-ng` (backend + app)
**Issue:** soul-player-ng#110

## Goal

Surface audiophile **loudness & dynamic-range** numbers per track — Integrated
LUFS, Loudness Range (LRA), true-peak (dBTP), and two dynamics figures (**PLR**
and **DR14**) — computed once per track by the existing background loudness scan
and shown in the now-playing signal-path detail plus a sortable library column.
The **DSP lives in the shared core** so the planned DAP head reuses it.

A **live** moving meter (short-term/momentary LUFS + PSR bars during playback) is
explicitly **out of scope** for this work — deferred to a follow-up. The research
shows real players (Roon, foobar2000, JRiver, Audirvāna, MusicBee) treat these as
**static per-track analysis numbers** shown in a detail surface / column, with live
meters being rare and plugin-only; the now-playing area shows at most a ReplayGain
figure (which we already ship).

## The metric set (exact definitions)

All per-track, from one whole-track analysis pass (BS.1770-4 / EBU R128 / Pleasurize):

| Metric | Definition | Standard | Notes |
|---|---|---|---|
| **Integrated LUFS** | Gated whole-programme loudness (abs −70 LUFS, rel −10 LU) | BS.1770-4 | Already computed (as a ReplayGain gain); expose the raw LUFS. |
| **LRA** (LU) | P95 − P10 of the gated **short-term** (3 s) loudness distribution (abs −70 LUFS, **rel −20 LU**) | EBU Tech 3342 | Needs short-term values; not meaningful < ~60 s. |
| **True-peak** (dBTP) | Max inter-sample peak via ≥4× oversampling | BS.1770-4 Annex 2 | On the **raw** (un-K-weighted) signal. |
| **PLR** (LU) | `true_peak_dbtp − integrated_lufs` | de-facto (MeterPlugs/TBProAudio) | Macro-dynamics; nearly free once we have TP + integrated. |
| **DR14** (integer) | Pleasurize/TT crest metric (per channel: loudest-20%-by-RMS energy-mean vs 2nd-highest 3 s-block peak, `20·log10`, mean over channels, round) | Pleasurize "Measuring DR" / dr14_t.meter | **Flat RMS, not K-weighted.** DR-Database-compatible. |

Dynamics bands (for colour coding, from the TT meter / Dynameter):
DR ≤ 7 / PLR low = red (brickwalled); 8–13 = yellow; ≥ 14 = green (dynamic).

## Architecture — all DSP in the core

```
soul-audio-core/soul-audio-loudness  (no_std, no-alloc, libm-free, O(1) memory)
    R128Analyzer (existing: integrated LUFS + sample-peak, histogram-gated)
      + short-term (3 s) loudness  → feeds LRA
      + LRA (short-term distribution, own −20 LU gate, P95−P10)   [bounded histogram]
      + true-peak (4×/2×/1× polyphase FIR by sample rate) → dBTP
      + DR14 (per-channel flat-RMS block histogram + top-2 peaks) [bounded]
      → one `LoudnessMetrics { integrated_lufs, lra, true_peak_dbtp, plr, dr }`
        computed from a single streaming `feed()` pass; PLR derived.
         │
soul-player-backend  (consumes core; the existing loudness scan)
    loudness.rs: analyze a track → `TrackLoudness` (the metric set + the RG gain)
    cache: `HashMap<PathBuf, TrackLoudness>` (extends today's gain-only cache)
    event: `Event::TrackLoudnessAnalyzed { path, metrics }`
         │
app/rune
    signal-path detail panel: a "Loudness" block for the now-playing track
    library: an optional sortable column (Integrated LUFS / DR)
    persistence: extend the on-disk loudness cache to the full metric set
```

### Core — bounded-memory constraints (critical)

The crate is `no_std`, **no-alloc**, **libm-free** (its own `crate::math`), and holds
**O(1) memory regardless of track length** (integrated uses a fixed-bin histogram).
Every addition must preserve this:

- **Short-term / LRA:** keep a 30-entry ring of 100 ms sub-block energies for the
  3 s short-term value; accumulate the short-term samples into a **second fixed-bin
  histogram** (same machinery as the integrated histogram) → percentiles P10/P95 by
  bin counting, with the −20 LU relative gate. O(1).
- **True-peak:** a small polyphase FIR state per channel (a dozen taps × 4 phases);
  running max. O(1). Oversample 4× < 96 kHz, 2× < 192 kHz, 1× ≥ 192 kHz.
- **DR14:** per channel, a **fixed-bin histogram of 3 s-block RMS** (for the loudest-20%
  energy-mean threshold + energy sum) and the **top-2 block peaks** (for the 2nd-highest
  peak). O(1) per channel. Flat (un-weighted) RMS with the +3.01 dB (factor-2) calibration.

Exact FIR coefficients, histogram bin widths, and the DR14 block length (`N = 3·(Fs+Δ)`,
Δ=60 only at 44.1 kHz) come from the implementation-research pass and go in the plan.

### Backend

- `soul_audio_loudness` already runs in `loudness.rs`'s background worker. Replace the
  single `replay_gain_db` output with the full `LoudnessMetrics` (the worker already
  decodes the whole track and feeds the analyzer — one pass yields everything).
- `LoudnessCache` value becomes `TrackLoudness { gain_db, integrated_lufs, lra,
  true_peak_dbtp, plr, dr }`. The RG gain path is unchanged (derived from integrated).
- `Event::TrackLoudnessAnalyzed` carries the metrics (was: just the gain).
- Keep the hybrid-ReplayGain behaviour intact — this is strictly additive.

### App

- **Signal-path detail panel** (where ReplayGain already shows): add a compact
  **Loudness** block for the now-playing track — Integrated LUFS, LRA, true-peak,
  PLR, DR (DR colour-banded). Values come from the loudness cache / the analyzed
  event; "—" / "analyzing…" until the scan completes.
- **Library column (optional, sortable):** Integrated LUFS and/or DR as a column in
  the Tracks view, read from the cache (no change to the catalog/index).
- **Persistence:** extend `config::{load,save}_loudness_cache` (today a `(path, gain)`
  map) to the full metric set, version-gated like the scan cache.

## Phasing

1. **Core PR** (soul-audio-core): the analyzer additions + `LoudnessMetrics`, TDD'd
   with the ffmpeg cross-check (LUFS/LRA/TP vs ffmpeg `ebur128`) and DR14 vs known
   reference values. Ship first; it's the shared DSP the DAP also gets.
2. **-ng PR** (soul-player-ng): bump core, extend the backend scan/cache/event, add
   the app signal-path block + library column + persistence.

## Testing

- **Core:** unit tests per metric (silence/sine calibration: 0 dBFS 997 Hz → −3.01 LUFS;
  LRA on a two-level signal; true-peak on an inter-sample-peak test vector; DR14 on a
  synthesized loudest-20% case). The existing `tests/ffmpeg_crosscheck.rs` validates
  LUFS/LRA/true-peak against ffmpeg within tolerance. DR14 validated against a couple of
  DR-Database-known files or the dr14_t.meter reference.
- **Backend:** the worker produces a populated `TrackLoudness`; cache round-trips.
- **App:** the signal-path block renders the metrics; "analyzing…" before the scan;
  the library column sorts.

## Out of scope (deferred)

- **Live moving meter** (short-term/momentary LUFS + live PSR bars at 10 Hz from the
  playback stream). Follow-up; the core short-term/true-peak added here are the
  foundation.
- **DSD** live/true-peak metering (exclude or decimate-to-PCM first; ultrasonic
  noise-shaping breaks K-weighting + true-peak).
