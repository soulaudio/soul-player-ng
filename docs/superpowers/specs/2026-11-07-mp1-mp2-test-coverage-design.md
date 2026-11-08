# MP1 / MP2 Test Coverage — Design

Date: 2026-11-07
Branch: `test/mp1-mp2-coverage`

## Problem

PR #3 added `mp2` and `mp1` to the library's `SUPPORTED` extensions and claimed
"support MP2/MP1". Only MP2 was actually exercised: there is a committed
`testdata/tone.mp2` and a `scan_lists_mp2_with_duration` unit test. **MP1 has no
fixture and no test anywhere in this repo** — the support is asserted but
unverified. This work finishes the MP1 side to match MP2, and extends coverage
across the committed test fixtures, the e2e suite, and the manual aria demo
library.

## Verified constraints

- **MP1 is not encodable here.** ffmpeg 8.0.1 exposes only `mp2` / `mp2fixed` /
  `libmp3lame` — there is no MPEG Layer I encoder. The only MP1 bytes in the
  project are the core conformance vector
  `soul-audio-core/testdata/tone-layer1.mp1` (mp1, 32 kHz stereo, 0.576 s,
  tagless, decodes cleanly). An **aria-content MP1 is therefore impossible**.
- **MP2 is encodable** (ffmpeg `-c:a mp2`). `testdata/tone.mp2` is a byte-for-byte
  copy of core's `tone.mp2` (mp2, 44.1 kHz stereo, 2.011 s).
- **Row titles can collide.** A track row renders `title ?? "Untitled"`, and the
  backend title falls back to the **filename stem** when a file is tagless. Two
  `tone.*` fixtures both render "tone", so an explicit per-format e2e assertion
  needs uniquely-named fixtures.
- **CI gates** Rust (`fmt` / `clippy -D warnings` / `cargo test` for both the
  workspace and `app/src-tauri`) and the frontend (`svelte-check` / `vitest` /
  `build`). The **wdio e2e is local-only** (not in CI); `tauri-driver.exe` is
  installed, so it can be run locally on Windows.

## Design

### Part A — `testdata/` + Rust unit test (closes the real gap)

1. Copy core `tone-layer1.mp1` → `testdata/tone.mp1` (naming matches the existing
   `tone.mp2` / `tone.mp3` family).
2. Add `scan_lists_mp1_with_duration` to
   `crates/soul-audio-service/src/library.rs`, mirroring
   `scan_lists_mp2_with_duration`: copy the fixture into a temp dir, scan, assert
   exactly one track with a nonzero duration. This is the test that proves the
   previously-untested MP1 support actually works end-to-end through the
   layer-aware tag scan.

### Part B — explicit per-format e2e

3. Add small, uniquely-named fixtures under `app/e2e/fixtures/mpeg-layers/`:
   - `layer-1.mp1` — copy of core `tone-layer1.mp1` (title renders "layer-1")
   - `layer-2.mp2` — copy of core `tone.mp2` (title renders "layer-2")
4. Add an e2e scenario that scans that folder via `load_folder_path` and asserts
   **both** named rows render, and that clicking one changes now-playing. Small
   fixtures keep it fast. The existing full-folder smoke test also picks up
   `testdata/tone.mp1` implicitly.

### Part C — aria demo library (`testing-audio-library/`)

A plain (non-git) folder of the same Aria recording across formats, named
`NN-aria.<ext>`, used for manual/demo testing (not automated).

5. Add `17-aria.mp2` — real aria content, encoded from `01-aria.wav`
   (`ffmpeg -c:a mp2 -b:a 192k`).
6. Add `18-layer1-tone.mp1` — the core conformance vector, copied in. It is
   **not** aria content (no aria-content MP1 is producible), hence the honest
   non-"aria" name.
7. Add `README.md` to the folder documenting the format list and the MP1-encoder
   caveat.

## Out of scope

- No changes to `soul-audio-core` (its fixtures already exist and are the source
  of truth; "fix core first" was explicitly not chosen).
- No backend DTO changes (track views deliberately carry no path/extension).
- No wiring of the wdio e2e into CI (it remains a local gate).

## Verification

- `cargo fmt --all --check` + the `app/src-tauri` manifest variant
- `cargo clippy --all-targets -D warnings` + the `app/src-tauri` variant
- `cargo test` (workspace) + `cargo test --manifest-path app/src-tauri/Cargo.toml`
  — includes the new `scan_lists_mp1_with_duration`
- Frontend `npm run check` / `npm run test` / `npm run build`
- `npm run e2e` (wdio) run locally
- `ffprobe` the two new aria fixtures to confirm codec/duration
