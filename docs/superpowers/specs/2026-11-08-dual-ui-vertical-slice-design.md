# Design: Dual-UI vertical slice (Rune vs Tauri+Svelte)

**Date:** 2026-11-08
**Branch:** `spike/rune-vs-tauri`
**Status:** Approved design — ready for implementation plan
**Companion:** [`docs/spikes/2026-11-08-rune-vs-tauri.md`](../../spikes/2026-11-08-rune-vs-tauri.md) (the "is Rune better?" evaluation that motivated this)

## Goal

Make the UI a swappable vertical slice so we can build a **Rune** head and the
existing **Tauri+Svelte** head against one shared backend contract, run them
side-by-side on the identical engine, and **pick one**. This is an
evaluate-then-decide spike: optimized to be cheap to build *and* cheap to delete.
It is **not** a commitment to maintain two front-ends long-term.

## Decisions (locked in brainstorming)

1. **Fat waist.** A single UI contract crate both heads bind to — sink selection,
   folder-open, and the player handle all live below the waist, so the only
   variable above it is the rendering technology.
2. **Naming.** The app's crates drop the misleading `soul-audio-` engine prefix
   (that belongs to the reusable engine in the *core* repo) for `soul-player-`:
   - `soul-audio-service` → **`soul-player-backend`** (the spine; renamed).
   - new **`soul-player-host`** (the waist).
3. **Layout.** `app/` becomes the heads directory: **`app/{tauri, rune}`**.
4. **Tauri stays put for the spike.** The Tauri head keeps its current
   `PlayerState`/IPC path; it is *not* migrated onto `soul-player-host` during the
   bake-off (both heads drive the same actor, so the comparison stays fair).
   Migrating Tauri onto the waist is an optional post-decision cleanup.

## Architecture — the hourglass

```
        app/rune (Rune)        app/tauri (Tauri+Svelte)   ← Layer 3: rendering (the variable)
             \                        /
        ┌────────────────────────────────┐
        │   soul-player-host              │               ← Layer 2: the fat waist (PlayerHandle)
        │   (sink pick · folder-open ·    │
        │    Library swap · event fan-out)│
        └────────────────────────────────┘
                      │
        soul-player-backend (DTOs + AudioPlayer actor)     ← Layer 1: the spine (cpal-free)
                      │
        soul-audio-core/* (engine, git dep)                ← Layer 0: untouched
```

Everything **below** the waist is byte-for-byte identical for both heads (fair
comparison). Everything **above** it is UI tech.

### Key design calls

- **`subscribe()` works after construction.** The `AudioPlayer` actor takes its
  `on_event` closure once at `spawn`. The host spawns it with a forwarder into a
  small internal fan-out, so a head can register its handler at startup. Rune's
  handler pushes events into a queue its frame loop drains; Tauri's would `emit`.
  One primitive, each head adapts.
- **`art()` returns raw bytes, not a base64 data-URL.** `Cover { bytes, mime }`
  crosses the waist raw. Tauri base64-encodes it to cross IPC (as it does today);
  Rune uploads the bytes straight to a GPU texture. Each head pays its own tax —
  this asymmetry is one of the things the bake-off measures.

## Crate responsibilities

### `crates/soul-player-backend` (renamed spine)
Unchanged behavior; only the name changes. Remains **cpal-free**,
**`forbid(unsafe)`** → still reusable on the firmware DAP. Owns: `Command` /
`Event` / `Tick` / `QueueState` / `TrackView` / `RepeatMode` DTOs, `apply_command`
/ `project_tick` / `project_queue`, the `AudioPlayer` actor, `Library`,
`cover_bytes` / `Cover`. Keeps all existing unit tests (they move with the rename).

### `crates/soul-player-host` (new waist)
Desktop assembly. Depends on `soul-player-backend` + `soul-audio-desktop`
(`playback`/cpal). **Its own nested cargo workspace** (like `app/src-tauri`),
*not* a root member — the repo convention is that cpal/platform-coupled crates
live in their own workspace, keeping the root workspace headless/GUI-free (the
`deny.toml` ban + `architecture.rs` gate, and the audio-lib-free root CI job, all
stay as-is). Root `members` therefore stays **explicit** (`["crates/soul-player-backend"]`),
never a `crates/*` glob that would vacuum host into the headless root. Host is
tested via its own `--manifest-path`. Re-exports the backend DTOs so a head
imports **one** crate.

```rust
// soul-player-host
pub use soul_player_backend::{Command, Event, Tick, QueueState, TrackView, RepeatMode, Cover};

pub struct PlayerHandle { /* owns AudioPlayer + current Library + sink choice + event fan-out */ }

impl PlayerHandle {
    /// Build over the real CpalSink, or NullSink when SOUL_PLAYER_OFFLINE is set.
    pub fn new() -> Self;
    /// Scan a folder, swap the Library, start playing from index 0; returns views.
    pub fn open_folder(&self, dir: &Path) -> anyhow::Result<Vec<TrackView>>;
    /// Current library views (no rescan).
    pub fn views(&self) -> Vec<TrackView>;
    /// Fire one transport/queue command (fire-and-forget).
    pub fn send(&self, cmd: Command);
    /// Raw cover art for a track id (bytes + mime), or None.
    pub fn art(&self, id: u32) -> Option<Cover>;
    /// Register an event handler (Tick ~8 Hz + Queue on mutation).
    pub fn subscribe(&self, on_event: impl Fn(Event) + Send + 'static);
    /// If SOUL_PLAYER_FOLDER is set, open_folder it. Shared launch harness.
    pub fn autoload_from_env(&self) -> anyhow::Result<()>;
}
```

The sink-selection + folder-open logic is **extracted from** today's
`app/src-tauri/src/{actor.rs, offline.rs, commands.rs}` (`build_actor`,
`offline_sink`, `load_folder`), which currently traps that UI-agnostic glue
inside the Tauri crate.

### `app/rune` (new head)
Own nested cargo workspace (mirrors `app/tauri/src-tauri`), so Rune's wgpu
dependency tree stays out of the root build and the head is trivially deletable.
Depends on `soul-player-host` by path. Pulls Rune pinned to a `dev` rev
(`rune = { git = "https://github.com/anomalyco/rune", rev = "<pin>" }`).

`main()`: `PlayerHandle::new()` → `autoload_from_env()` →
`subscribe(push into a frame-drained queue)` → Rune app with `view(&State)`. The
`State` struct mirrors the existing Svelte store 1:1 (`app/tauri/src/lib/store.svelte.ts`):
nowPlayingId, positionSecs, durationSecs, playing, volume, shuffle, repeat,
upNext, contextPos/Len, library.

## The Rune head slice — scope (ruthless)

**In** (covers every integration risk, nothing more):
- Scrollable track list (list rendering + data binding)
- Click a track → play (`send(PlayContext{ids, start})`)
- Now-playing bar: title/artist, position/duration progress, play·pause·next·prev
  (exercises the ~8 Hz `Tick` subscription + re-render cost)
- Seek + volume (continuous input → command)
- **Stretch:** cover art (`art()` bytes → GPU texture — the visual differentiator)

**Out** (do not move the verdict): updater, settings, queue-reorder UI, art
caching, native folder dialog, multi-window, mobile.

**Folder open:** Rune has no native dialog, so both heads load via the
`SOUL_PLAYER_FOLDER` env var at startup (reusing the pattern already in
`e2e_autoload`). `rfd` for a real dialog is an optional stretch.

## Shared launch harness + comparison method

Both heads boot identically:
- `SOUL_PLAYER_FOLDER=<dir>` → autoload at startup.
- `SOUL_PLAYER_OFFLINE=1` → `NullSink` (device-free; CI / headless screenshots).

Rune's headless-PNG render + element-tree dump lets us snapshot programmatically.

**Scorecard** (lands back in the companion spike doc as the final pick):

| Axis | Measure |
|---|---|
| DX / LOC | lines of Rust to rebuild the slice vs the Svelte views |
| Binary size | `app/rune` binary vs the Tauri bundle |
| Memory | idle + playing (no WebView2 process vs with) |
| Cold start | wall-clock to first frame |
| Render quality / feel | side-by-side screenshots (art, text, animation) |
| Iteration loop | edit → see-change time |
| Risk | pre-release dep + no-PR governance (already documented) |

## Testing

- **Backend:** existing service tests move with the rename, stay green.
- **Host:** ~2 unit tests over a `NullSink` + a tmp-dir WAV fixture — `open_folder`
  yields non-empty views; `subscribe` receives a `Tick` showing track 0 playing
  (mirrors the current `actor.rs` tests). A `PlayerHandle::offline()` test
  constructor forces the `NullSink` **without** mutating env (host `forbid`s
  unsafe, so no `set_var`). Run: `cargo test --manifest-path crates/soul-player-host/Cargo.toml`.
- **Rune head:** one headless smoke (boots, renders a frame); otherwise manual run.
- **Gate:** root `cargo test` green; host workspace tests green;
  `cd app/tauri && npm run tauri build` still works; `cd app/rune && cargo run`
  shows a working player.

## Build order (each step independently green)

1. **`git mv` `app/` → `app/tauri/`** (one isolated commit). Paths *internal* to
   the subtree are preserved (`../dist`, `../src/lib/bindings.ts`, the e2e→binary
   path). The **one external path that breaks**: `src-tauri/Cargo.toml`'s
   `path = "../../crates/soul-audio-service"` — once `src-tauri` is a level deeper
   it must become `../../../crates/...`. Then update the 6 outer references:
   `.github/dependabot.yml`, `.github/workflows/{ci,release,security,auto-release-on-version-bump}.yml`,
   `lefthook.yml` (+ prose in `README.md`, `docs/ARCHITECTURE.md`,
   `deny.app.toml` comment). Verify CI paths + local build.
2. **Rename `soul-audio-service` → `soul-player-backend`** (mechanical: crate
   dir, `Cargo.toml` name, `use soul_audio_service::` → `use soul_player_backend::`
   in `app/tauri/src-tauri/src/{commands,actor,ipc}.rs` + tests, root workspace
   member). Verify Tauri builds + all tests pass.
3. **Add `crates/soul-player-host`** with `PlayerHandle` (extract sink-selection +
   `open_folder` from the Tauri crate) + its unit tests. Green.
4. **Add `app/rune`** with the Rune slice over `PlayerHandle`. Runs.
5. **Measure → write up → pick.**

Steps 1–2 are pure, compiler-checked refactors that leave the shipping Tauri app
behavior-identical. Only steps 3–4 add new surface.

## Risks / non-goals

- **Rune is pre-release** (pinned `dev` rev, no releases, "no PRs" governance) —
  see the companion doc. This spike produces a *decision*, not a production
  commitment. If Tauri wins, the entire branch (move included) is discarded.
- **Four cargo workspaces** (root/backend + `crates/soul-player-host` +
  `app/tauri/src-tauri` + `app/rune`) means `soul-player-host`/`-backend` compile
  once per workspace that references them — the existing nested-workspace tradeoff,
  accepted for the spike to keep the root headless and the Rune deps isolated.
- **Non-goal:** unifying into a single workspace, or migrating the Tauri head onto
  `soul-player-host`. Both are deferred to "if Rune wins."
