# `soul-audio-dsp` — a shared DSP-chain + audio-profile crate

Date: 2026-11-15
Status: approved direction, pre-implementation

## Goal

Make the **DSP-chain model + the audio-profile format + the pure switching logic**
reusable by both heads:

- **`soul-player-ng`** (desktop) is the **authoring tool** — it *bakes* profiles: the
  switcher UI, editing, duplicate/rename/delete, persistence, export.
- **`soul-listener`** (the DAP; the Rust/Embassy firmware in `soul-listener-gh`) is the
  **playback target** — it *reads* a baked profile and applies it. Its DSP *processing*
  is already shared (`soul-audio-core::Equalizer` apply + `soul-audio-eq` design); what
  it can't do today is **read what -ng bakes**, because the chain model and the profile
  format are trapped in std-only desktop crates.

This crate closes that gap by making the model + format a single Rust definition both
heads compile against — so a profile exported on desktop is readable on the DAP *by
construction*, with no second implementation to drift.

## The boundary (what moves, what stays)

| Concern | Home | Why |
|---|---|---|
| Chain model `DspChain` / `DspUnit` / `EqUnit` / `DspKind` | **→ `soul-audio-dsp`** | Pure data (`Vec`/`String`/`f32`); only `DspSink` kept them in the host |
| Profile model `Profile` / `ProfileSet` + switching (`switch`, `add`, `duplicate`, `remove`, `rename`, write-through, unique-name) | **→ `soul-audio-dsp`** | Pure state transitions — identical on both heads; "DAP can reuse switching" |
| Serialized format (`ProfileCfg` / `DspUnitCfg` / `BandCfg`, JSON read/write) | **→ `soul-audio-dsp`** (serde feature) | The format *is* the interop contract |
| Per-sample apply (`Biquad`, `Equalizer`) | stays `soul-audio-core` | Already shared, no-alloc kernel |
| Coefficient design + `Band`/`BandKind` + AutoEQ text | stays `soul-audio-eq` | Already shared; the new crate depends on it |
| `DspSink` (pump thread, `Arc<Mutex>`, spectrum tap) | stays `soul-player-host` | std engine glue; imports the chain type from the new crate |
| `config.toml` persistence, file dialogs | stays `soul-player-ng` | Desktop filesystem; the DAP persists to its own storage |
| The settings **UI** (profile bar, dropdowns, pipeline page) | stays `soul-player-ng` | Rune desktop ≠ the DAP's e-ink display |

Net: the DAP reuses **model + format + switching + processing**; -ng keeps **UI +
persistence I/O + the decision to author**. Each head is a thin adapter: it holds a
`ProfileSet`, and on each mutation does its own *persist* + *engine-push*.

## The crate (`crates/soul-audio-dsp`, in the soul-audio-core repo)

Tier **T1 / NoStdAlloc** (like `soul-audio-queue` and `soul-audio-eq`). Template =
`soul-audio-eq`; serde pattern = `soul-audio-queue` (serde is an **optional feature**, so
the default/no-default `thumbv7em-none-eabihf` surface stays within the documented T1
external set `{embedded-io, libm}`).

```toml
# crates/soul-audio-dsp/Cargo.toml
[package]
name = "soul-audio-dsp"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
description = "Portable no_std DSP-chain + audio-profile model (typed DSP units, profiles, switching, and the exchange format), shared by the desktop host and the DAP firmware. Applies through soul-audio-core's Equalizer; designs via soul-audio-eq."

[lints]
workspace = true

[features]
default = []
alloc = []
std = ["alloc"]
serde = ["dep:serde", "alloc", "soul-audio-eq?/serde"]  # see Band-serde note

[dependencies]
soul-audio-eq = { path = "../soul-audio-eq" }

[dependencies.serde]
version = "1"
optional = true
default-features = false
features = ["derive", "alloc"]
```

```rust
// src/lib.rs
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
extern crate alloc;
```

### Public surface

- **Model** (moved verbatim from `soul-player-host::eq`):
  `EqUnit { bands: Vec<Band>, preamp_db, enabled, name: Option<String> }`,
  `enum DspKind { Eq }`, `enum DspUnit { Eq(EqUnit) }` (all the accessors),
  `struct DspChain { units: Vec<DspUnit>, generation: u64 }`.
- **Profiles** (new, pure):
  - `struct Profile { name: String, chain: DspChain }`
  - `struct ProfileSet { profiles: Vec<Profile>, active: usize }` with
    `active()`, `active_chain()`, `switch(i)`, `new_empty(name)`, `duplicate_active()`,
    `remove_active()` (guards ≥1 → returns bool), `rename(i, name)`,
    `set_active_chain(chain)` (the auto-save write-through), `unique_name(base)`.
    All pure; no I/O, no engine handle.
- **Format** (behind the `serde` feature): `BandCfg`, `DspUnitCfg` (string `kind` tag +
  forward-compat: unknown kind → skipped), `ProfileCfg { name, chain: Vec<DspUnitCfg> }`,
  and conversions `to_/from_` the runtime model. These are the current
  `app/rune/src/config.rs` structs, moved down. JSON helpers (`profile_to_json` /
  `profile_from_json`) move too; the *serializer* (serde_json on desktop, postcard/json
  on the DAP) stays per-platform — only the struct shapes are shared.

**Band-serde note:** `soul-audio-eq::Band`/`BandKind` carry no serde derives today. The
format path doesn't need them — `DspUnitCfg`/`BandCfg` are the serialized form and own the
string-token `kind` mapping (keeping the TOML/JSON forward-compatible). So the crate's
`serde` feature does **not** require `soul-audio-eq/serde`; drop that optional edge unless
a later need to serialize a raw `Band` appears.

## Architecture-test / gate changes (mandatory — core CI goes red otherwise)

1. `crates/architecture-tests/tests/tiers.rs` → add `("soul-audio-dsp", Tier::NoStdAlloc)`
   to `MANIFEST` (else `every_crate_is_classified` fails). The internal edge
   `soul-audio-dsp → soul-audio-eq` is already legal (`NoStdAlloc → NoStdAlloc`).
   T1 has **no external-dep allowlist test**, and serde is **optional**, so the dependency
   checks pass.
2. `docs/ARCHITECTURE.md` §3 → list `soul-audio-dsp` in the T1 members row (doc-sync
   convention; keeps §3's `{embedded-io, libm}` wording honest since serde stays optional).
3. `xtask/src/main.rs::verify()` → add a
   `build -p soul-audio-dsp --target thumbv7em-none-eabihf --no-default-features` line, and
   add `soul-audio-dsp` to `gate.rs::SHARED_CRATES` so a change to it forces the full gate.
4. The crate's own unit tests (round-trip, switching, migration, no-alloc apply path are
   unaffected). `cargo xtask ci` is the local gate before the core PR.

## Migration sequence (two repos, three PRs)

**PR-1 (core repo, in a worktree — never the main checkout):** create `soul-audio-dsp`,
move the model, add `Profile`/`ProfileSet` + the format, wire the tier manifest + docs +
xtask gate, TDD the crate, `cargo xtask ci`, merge to core `main`.

**PR-2 (`soul-player-ng`): consume it.**
- Add `soul-audio-dsp = { git = …, branch = "main", features = ["std","serde"] }` to
  `soul-player-host` (and the app where it needs the format). Bump the pinned core rev in
  **all three workspaces'** lockfiles (`cargo update -p soul-audio-dsp` etc.) per the
  lockfile rule.
- `soul-player-host/src/eq.rs`: delete the moved definitions; `pub use
  soul_audio_dsp::{DspChain, DspUnit, DspKind, EqUnit}`; keep `DspSink`/`DspShared` and
  `set_dsp_chain`/`dsp_chain`.
- `app/rune/src/config.rs`: drop the local `ProfileCfg`/`DspUnitCfg`/`BandCfg`; use the
  crate's. `load_profiles`/`save_profiles` keep doing **file I/O**, over the shared format.
- `app/rune/src/main.rs`: `State` holds a `soul_audio_dsp::ProfileSet`; the profile
  mutators become thin wrappers — call the `ProfileSet` method, then `config::save` +
  `host.set_dsp_chain`. Behavior and tests are preserved.

**PR-3 (future, `soul-listener`): the DAP consumes it** — reads a profile file into the
shared model, applies via the shared processing, selects active via `ProfileSet::switch`.
Out of scope now (the DAP doesn't depend on core yet; a migration plan exists in
`soul-listener-gh`).

## Future roadmap (enabled by this; NOT built here)

The user's longer arc, which this crate is the foundation for:

- **Device detection** — desktop notices a `soul-listener` is connected (USB-C UAC2 /
  BLE companion, per the DAP README roadmap).
- **Firmware update** — if a newer firmware exists, flash it (OTA/DFU; none exists in the
  DAP today — it would be new on the firmware side).
- **Profile import into the DAP** — push baked audio profiles (and later library/art)
  onto the device over that transport. This crate guarantees the pushed file is readable;
  the *transport* itself stays deferred until `soul-listener` consumes core.

These are separate programs; this spec only delivers the shared crate + the -ng refactor.

## Non-goals

- No change to the per-sample apply or EQ design (already shared).
- No DAP transport / detection / flashing in this work.
- No UI or persistence moved out of -ng (authoring stays desktop-side).
- Don't derive serde on `soul-audio-eq::Band` unless a concrete need appears.

## Risks / notes

- **Core-repo process**: conserving CI → verify locally with `cargo xtask ci`; push
  `--no-verify`; don't run -ng cargo while core's pre-push verify runs (shared `~/.cargo`
  lock). See the core-repo memories.
- **Lockfile fan-out**: a new core crate means updating all three -ng workspace lockfiles.
- **Format stability**: once the DAP reads this format, the `DspUnitCfg` shape becomes a
  compatibility surface — keep the string-token `kind` + unknown-skip so new unit kinds
  (future compressor/limiter) are additive.
