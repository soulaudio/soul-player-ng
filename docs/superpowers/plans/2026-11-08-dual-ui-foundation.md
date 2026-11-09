# Dual-UI Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure the repo into `app/{tauri,rune}`, rename the headless backend crate to `soul-player-backend`, and add a shared `soul-player-host` crate exposing the `PlayerHandle` UI contract — the foundation both the Tauri and Rune heads will bind to.

**Architecture:** An hourglass. `soul-player-backend` (DTOs + `AudioPlayer` actor, cpal-free) is the spine; `soul-player-host` is the fat waist (picks the sink, opens folders, owns the `Library`, fans out events) exposing one `PlayerHandle`. This plan builds the spine-rename + waist; the Rune head and the Tauri↔Rune bake-off are a follow-up plan.

**Tech Stack:** Rust (edition 2024), cargo workspaces, `soul-audio-core/*` (git deps), cpal via `soul-audio-desktop` (`playback`), `anyhow`, `tempfile` (dev).

**Scope:** This plan covers spec build-order steps 1–3 only. Steps 4–5 (the Rune head over `PlayerHandle`, then the measured comparison) are authored as a **separate** plan after this lands — Rune is a pre-release, unversioned dependency, so its UI API must be read from its own `examples/` at that point rather than guessed here. This plan produces working, testable software on its own: a green repo with the new structure and a reusable, unit-tested `soul-player-host`.

Source spec: `docs/superpowers/specs/2026-11-08-dual-ui-vertical-slice-design.md`.

## Global Constraints

- **Commit messages:** Conventional Commits; **no** AI/attribution trailers (a `commit-msg` hook rejects "Claude/Anthropic/AI/Co-Authored-By"). One line, `type(scope): subject`.
- **Backend crate stays cpal-free and `forbid(unsafe_code)`** — firmware-reusable. Do not add device/GUI deps to it.
- **Root workspace stays headless/GUI-free.** Its `deny.toml` bans `tauri`/`wry`/`tao`; `crates/.../tests/architecture.rs` asserts the same. Do **not** add `soul-player-host` (cpal) or any head to the root `members` — host is its own nested workspace; root `members` stays the explicit single entry.
- **Edition 2024, `rust-version = "1.85"`, `license = "MIT OR Apache-2.0"`** for new crates.
- **Git deps** are pinned to `https://github.com/soulaudio/soul-audio-core`, `branch = "main"`.
- **Never hand-edit `Cargo.lock`** — cargo regenerates it on build.
- Run commands from the worktree root unless a step says otherwise.

---

### Task 1: Relocate `app/` → `app/tauri/`

Move the entire Tauri+Svelte head under `app/tauri/` so `app/` becomes the heads
directory. Pure path refactor — no behavior change. The regression "test" is that
the existing suites still pass from the new paths.

**Files:**
- Move: `app/` → `app/tauri/` (whole subtree: `package.json`, `src/`, `src-tauri/`, `e2e/`, config)
- Modify: `app/tauri/src-tauri/Cargo.toml` (the one external path dep)
- Modify: `.github/dependabot.yml`, `.github/workflows/{ci,release,security,auto-release-on-version-bump}.yml`, `lefthook.yml`
- Modify (prose): `README.md`, `docs/ARCHITECTURE.md`, `deny.app.toml`

**Interfaces:**
- Consumes: nothing.
- Produces: the new path `app/tauri/src-tauri/Cargo.toml` for Task 2; the layout `app/tauri` for the eventual `app/rune` sibling.

- [ ] **Step 1: Remove regenerable build artifacts (keeps the move clean)**

```bash
rm -rf app/node_modules app/dist app/src-tauri/target app/e2e/node_modules
```

- [ ] **Step 2: Move the subtree via a temp rename (avoids moving a dir into itself)**

```bash
git mv app app_tmp
mkdir app
git mv app_tmp app/tauri
git status --short | head
```
Expected: renames staged, e.g. `R  app/src-tauri/... -> app/tauri/src-tauri/...`.

- [ ] **Step 3: Fix the one external relative path the move broke**

`app/tauri/src-tauri/Cargo.toml` — the path dep points up to the repo-root
`crates/`, which is now one level further up. Change:

```toml
soul-audio-service = { path = "../../crates/soul-audio-service", features = ["specta"] }
```
to:
```toml
soul-audio-service = { path = "../../../crates/soul-audio-service", features = ["specta"] }
```
(The name is still `soul-audio-service` here — Task 2 renames it.)

- [ ] **Step 4: Update CI/config path references**

In the 6 files below, replace these path fragments (prefix `app/` → `app/tauri/`):
- `app/src-tauri` → `app/tauri/src-tauri`
- `app/e2e` → `app/tauri/e2e`
- `app/package-lock.json` → `app/tauri/package-lock.json`
- `working-directory: app` → `working-directory: app/tauri`
- `--prefix app ` → `--prefix app/tauri ` (in `lefthook.yml` pre-push `svelte-check`)

Files: `.github/dependabot.yml`, `.github/workflows/ci.yml`,
`.github/workflows/release.yml`, `.github/workflows/security.yml`,
`.github/workflows/auto-release-on-version-bump.yml`, `lefthook.yml`.

Convenience (bash) — review the diff after:
```bash
for f in .github/dependabot.yml .github/workflows/ci.yml .github/workflows/release.yml \
         .github/workflows/security.yml .github/workflows/auto-release-on-version-bump.yml lefthook.yml; do
  sed -i \
    -e 's#app/src-tauri#app/tauri/src-tauri#g' \
    -e 's#app/e2e#app/tauri/e2e#g' \
    -e 's#app/package-lock.json#app/tauri/package-lock.json#g' \
    -e 's#working-directory: app$#working-directory: app/tauri#g' \
    -e 's#--prefix app #--prefix app/tauri #g' \
    "$f"
done
```

- [ ] **Step 5: Update prose path references**

In `README.md` (`cd app` → `cd app/tauri`), `docs/ARCHITECTURE.md` (`app/src` →
`app/tauri/src`, `app/src-tauri` → `app/tauri/src-tauri`), and the
`deny.app.toml` header comment (`app/src-tauri` → `app/tauri/src-tauri`).

- [ ] **Step 6: Verify no stale `app/` paths remain and manifests resolve**

```bash
grep -rn -E "app/src-tauri|app/e2e|working-directory: app$|--prefix app " \
  .github lefthook.yml README.md docs/ARCHITECTURE.md deny.app.toml | grep -v "app/tauri" || echo "clean"
cargo metadata --manifest-path app/tauri/src-tauri/Cargo.toml --format-version 1 >/dev/null && echo "tauri manifest resolves"
cargo metadata --format-version 1 >/dev/null && echo "root manifest resolves"
```
Expected: `clean`, `tauri manifest resolves`, `root manifest resolves`.

- [ ] **Step 7: Verify the root (backend) suite still passes**

```bash
cargo test
```
Expected: PASS (backend tests unchanged; the relocation didn't touch them).

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor(app): relocate the tauri head to app/tauri"
```

---

### Task 2: Rename `soul-audio-service` → `soul-player-backend`

Mechanical, compiler-checked rename. The crate's behavior is unchanged; only its
name/import path change. Drops the misleading `soul-audio-` engine prefix.

**Files:**
- Move: `crates/soul-audio-service/` → `crates/soul-player-backend/`
- Modify: `crates/soul-player-backend/Cargo.toml` (package name)
- Modify: `Cargo.toml` (root `members`)
- Modify: `app/tauri/src-tauri/Cargo.toml` (dep name + path + profile key)
- Modify: `app/tauri/src-tauri/src/{commands,actor,ipc}.rs`
- Modify: `app/tauri/src-tauri/tests/{common/mod.rs,e2e.rs,fixtures.rs,foundation.rs}`
- Modify: `crates/soul-player-backend/examples/art_check.rs`
- Modify (prose): `README.md`, `app/tauri/README.md`, `docs/ARCHITECTURE.md`, `lefthook.yml`, `deny.toml`, `.github/dependabot.yml`, `.github/workflows/security.yml`, `crates/soul-player-backend/tests/architecture.rs`, `app/tauri/e2e/test/specs/player.e2e.ts`

**Interfaces:**
- Consumes: the `app/tauri` layout from Task 1.
- Produces: crate `soul-player-backend` (lib name `soul_player_backend`) re-exporting `Command`, `Event`, `Tick`, `QueueState`, `TrackView`, `RepeatMode`, `PlayState`, `AudioPlayer`, `Library`, `Cover`, `cover_bytes`, `apply_command`, `project_tick`, `project_queue` — used by Task 3 (`soul-player-host`).

- [ ] **Step 1: Move the crate directory**

```bash
git mv crates/soul-audio-service crates/soul-player-backend
```

- [ ] **Step 2: Rename the package**

`crates/soul-player-backend/Cargo.toml` line 2:
```toml
name = "soul-player-backend"
```

- [ ] **Step 3: Update the root workspace member (keep it explicit)**

`Cargo.toml` (root) line 3:
```toml
members = ["crates/soul-player-backend"]
```
Do **not** change this to `crates/*` — that would pull the cpal-carrying
`soul-player-host` (Task 3) into the headless root workspace.

- [ ] **Step 4: Update the Tauri crate's dependency + profile key**

`app/tauri/src-tauri/Cargo.toml`:
```toml
soul-player-backend = { path = "../../../crates/soul-player-backend", features = ["specta"] }
```
and the dev profile override key:
```toml
[profile.dev.package.soul-player-backend]
opt-level = 2
```
Also update the package `description` string (`...over soul-audio-service.` →
`...over soul-player-backend.`).

- [ ] **Step 5: Update all Rust `use`/path references**

Replace every `soul_audio_service` with `soul_player_backend` across the Tauri
crate's source, tests, and the backend crate's example:

```bash
grep -rl "soul_audio_service" app/tauri/src-tauri crates/soul-player-backend | while read -r f; do
  sed -i 's/soul_audio_service/soul_player_backend/g' "$f"
done
```
This covers `app/tauri/src-tauri/src/{commands,actor,ipc}.rs`,
`app/tauri/src-tauri/tests/{common/mod.rs,e2e.rs,fixtures.rs,foundation.rs}`, and
`crates/soul-player-backend/examples/art_check.rs` (both its `use` and the
`cargo run -p soul-audio-service` in its doc comment → `-p soul-player-backend`).

- [ ] **Step 6: Update prose / comments**

Replace the hyphenated name in docs and comments (not compiler-checked, so do it
explicitly): `README.md`, `app/tauri/README.md`, `docs/ARCHITECTURE.md` (incl.
the `crates/soul-audio-service/tests/architecture.rs` path → `crates/soul-player-backend/tests/architecture.rs`),
`lefthook.yml`, `deny.toml`, `.github/dependabot.yml`,
`.github/workflows/security.yml`, the prose strings in
`crates/soul-player-backend/tests/architecture.rs` (doc comment + assert message),
and the comment in `app/tauri/e2e/test/specs/player.e2e.ts`.

```bash
grep -rl "soul-audio-service" README.md app/tauri/README.md docs/ARCHITECTURE.md \
  lefthook.yml deny.toml .github crates/soul-player-backend/tests/architecture.rs \
  app/tauri/e2e/test/specs/player.e2e.ts | while read -r f; do
  sed -i 's/soul-audio-service/soul-player-backend/g' "$f"
done
```
(Leave `docs/spikes/*` and `docs/superpowers/specs/*` as historical record.)

- [ ] **Step 7: Verify no code references to the old name remain**

```bash
grep -rn -E "soul_audio_service|soul-audio-service" \
  --include=*.rs --include=*.toml . | grep -v -E "target/|/specs/|/spikes/" || echo "clean"
```
Expected: `clean`.

- [ ] **Step 8: Build + test both workspaces**

```bash
cargo test
cargo test --manifest-path app/tauri/src-tauri/Cargo.toml
```
Expected: both PASS (including `headless_service_has_no_gui_dependencies` in the
backend and the Tauri Tier-1 tests). `bindings.ts` regenerates on the Tauri debug
build and will reflect the renamed `soul_player_backend::cover_bytes` doc path.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(backend): rename soul-audio-service to soul-player-backend"
```

---

### Task 3: Scaffold `soul-player-host` + `PlayerHandle` skeleton

Create the waist crate as its own nested workspace, with the sink module and the
`PlayerHandle` plumbing (construction, subscribe, lazy actor, send, views, art,
autoload). Folder-open lands in Task 4; the event-flow test in Task 5.

**Files:**
- Create: `crates/soul-player-host/Cargo.toml`
- Create: `crates/soul-player-host/src/lib.rs`
- Create: `crates/soul-player-host/src/sink.rs`

**Interfaces:**
- Consumes: `soul_player_backend::{AudioPlayer, Command, Event, Library, Cover, TrackView, cover_bytes}` (Task 2).
- Produces:
  - `pub struct PlayerHandle`
  - `pub enum SinkMode { Device, Offline }`
  - `PlayerHandle::new() -> Self`, `PlayerHandle::offline() -> Self`, `PlayerHandle::with_sink_mode(SinkMode) -> Self`
  - `PlayerHandle::subscribe(&self, impl Fn(Event) + Send + 'static)`
  - `PlayerHandle::send(&self, Command)`
  - `PlayerHandle::views(&self) -> Vec<TrackView>`
  - `PlayerHandle::art(&self, u32) -> Option<Cover>`
  - `PlayerHandle::autoload_from_env(&self) -> anyhow::Result<()>`
  - `PlayerHandle::open_folder(&self, &Path) -> anyhow::Result<Vec<TrackView>>` (implemented in Task 4)
  - re-exports: `Command, Event, Tick, QueueState, TrackView, RepeatMode, Cover`

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "soul-player-host"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "MIT OR Apache-2.0"
publish = false
description = "Desktop assembly over soul-player-backend: sink selection, folder-open, and the PlayerHandle UI contract every head binds to."

[workspace]
resolver = "3"

[lints.rust]
unsafe_code = "forbid"

[lints.clippy]
all = { level = "deny", priority = -1 }

[dependencies]
soul-player-backend = { path = "../soul-player-backend" }
soul-audio-core = { git = "https://github.com/soulaudio/soul-audio-core", branch = "main" }
soul-audio-desktop = { git = "https://github.com/soulaudio/soul-audio-core", branch = "main", features = ["playback"] }
anyhow = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write `src/sink.rs` (sink selection + NullSink)**

```rust
//! Audio sink selection for the desktop host: the real `CpalSink`, or a headless
//! `NullSink` for device-free runs and unit tests. Mirrors the sink choice that
//! previously lived in the Tauri crate's `actor.rs`/`offline.rs`.
#![forbid(unsafe_code)]

use std::sync::Arc;

use soul_audio_core::{SampleFormat, Sink, SinkError, StreamInfo};
use soul_audio_desktop::sink::CpalSink;
use soul_player_backend::{AudioPlayer, Event, Library};

/// Which sink the actor plays through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkMode {
    /// The real audio device (`CpalSink`).
    Device,
    /// A discard sink — no device, no backpressure. For `SOUL_PLAYER_OFFLINE`
    /// runs and tests.
    Offline,
}

impl SinkMode {
    /// `Offline` iff `SOUL_PLAYER_OFFLINE` is set, else `Device`.
    pub fn from_env() -> Self {
        if std::env::var_os("SOUL_PLAYER_OFFLINE").is_some() {
            SinkMode::Offline
        } else {
            SinkMode::Device
        }
    }
}

/// Spawn the backend actor over the sink `mode` selects. The sink factory runs on
/// the pump thread, so a `!Send` device sink is fine.
pub(crate) fn build_actor(
    mode: SinkMode,
    lib: Arc<Library>,
    on_event: impl Fn(Event) + Send + 'static,
) -> AudioPlayer {
    match mode {
        SinkMode::Offline => AudioPlayer::spawn(|| Ok(NullSink::new(48_000, 2)), lib, on_event),
        SinkMode::Device => AudioPlayer::spawn(CpalSink::new, lib, on_event),
    }
}

/// A [`Sink`] that discards output — no device, no backpressure.
pub struct NullSink {
    rate: u32,
    channels: u16,
}

impl NullSink {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self { rate, channels }
    }
}

impl Sink for NullSink {
    fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            sample_rate: self.rate,
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }
    fn write(&mut self, _frames: &[i32]) -> Result<(), SinkError> {
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
}
```

- [ ] **Step 3: Write `src/lib.rs` (PlayerHandle plumbing, minus open_folder)**

```rust
//! Desktop assembly over `soul-player-backend`: picks the audio sink, owns the
//! live `Library`, fans backend events out to subscribers, and exposes the one
//! `PlayerHandle` every UI head (Tauri, Rune) binds to.
#![forbid(unsafe_code)]

mod sink;

use std::path::Path;
use std::sync::{Arc, Mutex};

use soul_player_backend::{AudioPlayer, Library, cover_bytes};

pub use sink::SinkMode;
pub use soul_player_backend::{Command, Cover, Event, QueueState, RepeatMode, Tick, TrackView};

type Subscriber = Box<dyn Fn(Event) + Send>;

struct Inner {
    mode: SinkMode,
    player: Mutex<Option<AudioPlayer>>,
    library: Mutex<Option<Arc<Library>>>,
    subscribers: Mutex<Vec<Subscriber>>,
}

/// The one UI contract both heads bind to. Owns the backend actor (spawned lazily
/// on first playback), the current library, and an event fan-out.
pub struct PlayerHandle {
    inner: Arc<Inner>,
}

impl Default for PlayerHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerHandle {
    /// Build with the sink `SinkMode::from_env()` picks (Device, or Offline when
    /// `SOUL_PLAYER_OFFLINE` is set).
    pub fn new() -> Self {
        Self::with_sink_mode(SinkMode::from_env())
    }

    /// Build forced to the headless `NullSink` (tests / device-free smoke) with
    /// no env mutation.
    pub fn offline() -> Self {
        Self::with_sink_mode(SinkMode::Offline)
    }

    /// Build with an explicit sink mode.
    pub fn with_sink_mode(mode: SinkMode) -> Self {
        Self {
            inner: Arc::new(Inner {
                mode,
                player: Mutex::new(None),
                library: Mutex::new(None),
                subscribers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Register an event handler. Called on every backend event (Tick ~8 Hz +
    /// Queue on mutation), from the pump thread. Handlers must not call back into
    /// this handle. Works before or after the actor has started.
    pub fn subscribe(&self, on_event: impl Fn(Event) + Send + 'static) {
        self.inner
            .subscribers
            .lock()
            .expect("subscribers mutex")
            .push(Box::new(on_event));
    }

    /// Current library views (empty until a folder is opened).
    pub fn views(&self) -> Vec<TrackView> {
        self.inner
            .library
            .lock()
            .expect("library mutex")
            .as_ref()
            .map(|l| l.views())
            .unwrap_or_default()
    }

    /// Raw cover art for a track id (bytes + mime), or `None`.
    pub fn art(&self, id: u32) -> Option<Cover> {
        let lib = self.inner.library.lock().expect("library mutex").clone()?;
        let path = lib.path_of(id)?.to_owned();
        cover_bytes(&path)
    }

    /// Fire one transport/queue command (fire-and-forget). No-op until a folder
    /// has been opened (the actor needs a library to start).
    pub fn send(&self, cmd: Command) {
        self.ensure_actor();
        if let Some(actor) = self.inner.player.lock().expect("player mutex").as_ref() {
            actor.send(cmd);
        }
    }

    /// If `SOUL_PLAYER_FOLDER` is set, open it. The shared launch harness for both
    /// heads.
    pub fn autoload_from_env(&self) -> anyhow::Result<()> {
        if let Some(dir) = std::env::var_os("SOUL_PLAYER_FOLDER") {
            self.open_folder(Path::new(&dir))?;
        }
        Ok(())
    }

    /// Swap the library the actor resolves ids against (and cache it app-side).
    /// Locks library then player, never both at once, to avoid a lock-order
    /// inversion with `ensure_actor` (which snapshots library before locking
    /// player).
    fn set_library(&self, lib: Arc<Library>) {
        {
            *self.inner.library.lock().expect("library mutex") = Some(Arc::clone(&lib));
        }
        if let Some(actor) = self.inner.player.lock().expect("player mutex").as_ref() {
            actor.set_library(lib);
        }
    }

    /// Lazily spawn the actor once a library exists. Snapshots the library WITHOUT
    /// holding the player lock (avoids inverting `set_library`'s library→player
    /// order). The forwarder fans each event out to the current subscribers.
    fn ensure_actor(&self) {
        let lib = { self.inner.library.lock().expect("library mutex").clone() };
        let Some(lib) = lib else {
            return; // no folder opened yet
        };
        let mut guard = self.inner.player.lock().expect("player mutex");
        if guard.is_some() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let forward = move |ev: Event| {
            for sub in inner.subscribers.lock().expect("subscribers mutex").iter() {
                sub(ev.clone());
            }
        };
        *guard = Some(sink::build_actor(self.inner.mode, lib, forward));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_handle_has_empty_views_before_open() {
        let h = PlayerHandle::offline();
        assert!(h.views().is_empty());
    }

    #[test]
    fn send_before_open_is_a_noop() {
        let h = PlayerHandle::offline();
        h.send(Command::TogglePlay); // no library yet → no actor → no panic
        assert!(h.views().is_empty());
    }
}
```

Note: `src/lib.rs` references `self.open_folder(...)` in `autoload_from_env`.
`open_folder` is added in Task 4, so **this step will not compile until Task 4**.
To keep Task 3 independently green, add a temporary stub immediately after the
`with_sink_mode` method, to be replaced in Task 4:

```rust
    /// (Task 4 replaces this stub with the real scan→play implementation.)
    pub fn open_folder(&self, _dir: &Path) -> anyhow::Result<Vec<TrackView>> {
        anyhow::bail!("open_folder not yet implemented")
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --manifest-path crates/soul-player-host/Cargo.toml
```
Expected: PASS (`offline_handle_has_empty_views_before_open`,
`send_before_open_is_a_noop`). First build fetches the git deps + compiles cpal.

- [ ] **Step 5: Commit**

```bash
git add crates/soul-player-host
git commit -m "feat(host): scaffold soul-player-host with the PlayerHandle skeleton"
```

---

### Task 4: `PlayerHandle::open_folder`

Replace the Task 3 stub with the real scan → swap-library → start-playing flow
(extracted from the Tauri crate's `load_folder`).

**Files:**
- Modify: `crates/soul-player-host/src/lib.rs`

**Interfaces:**
- Consumes: `PlayerHandle` plumbing + `Library::scan`/`views`/`path_of`, `Command::PlayContext` (Task 3 / backend).
- Produces: working `PlayerHandle::open_folder(&self, &Path) -> anyhow::Result<Vec<TrackView>>` (same signature as the stub) for Task 5 and the heads.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/soul-player-host/src/lib.rs`:

```rust
    /// Minimal stereo-16-bit WAV (mirrors the backend actor tests' writer).
    fn wav(rate: u32, frames: u32) -> Vec<u8> {
        let channels: u16 = 2;
        let block_align = channels * 2;
        let byte_rate = rate * u32::from(block_align);
        let data_len = frames * u32::from(block_align);
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        v
    }

    #[test]
    fn open_folder_scans_and_exposes_views() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.wav"), wav(48_000, 96_000)).unwrap();
        std::fs::write(dir.path().join("b.wav"), wav(48_000, 96_000)).unwrap();

        let h = PlayerHandle::offline();
        let views = h.open_folder(dir.path()).expect("open_folder");
        assert_eq!(views.len(), 2, "both wavs scanned");
        assert_eq!(h.views().len(), 2, "views() reflects the opened library");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --manifest-path crates/soul-player-host/Cargo.toml open_folder_scans_and_exposes_views
```
Expected: FAIL (`open_folder not yet implemented`).

- [ ] **Step 3: Replace the stub with the real implementation**

In `crates/soul-player-host/src/lib.rs`, replace the Task 3 `open_folder` stub
with:

```rust
    /// Scan `dir` into a `Library`, swap it in, and start playing from track 0.
    /// Returns the track views. Errors on scan failure or an empty folder.
    pub fn open_folder(&self, dir: &Path) -> anyhow::Result<Vec<TrackView>> {
        let lib = Arc::new(Library::scan(dir)?);
        let views = lib.views();
        if views.is_empty() {
            anyhow::bail!("no supported audio files in {}", dir.display());
        }
        self.set_library(Arc::clone(&lib));
        let ids: Vec<u32> = views.iter().map(|v| v.id).collect();
        self.send(Command::PlayContext { ids, start: 0 });
        Ok(views)
    }
```

No new imports are needed: `Library` and `Arc` are already in scope from Task 3's
`use` lines, and `Command`/`TrackView` are in scope via the `pub use`
re-export. Do **not** re-add them (a duplicate import fails to compile).

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test --manifest-path crates/soul-player-host/Cargo.toml
```
Expected: PASS (all three host tests).

- [ ] **Step 5: Commit**

```bash
git add crates/soul-player-host/src/lib.rs
git commit -m "feat(host): implement PlayerHandle::open_folder (scan, swap, play)"
```

---

### Task 5: Event fan-out (end-to-end `subscribe`)

Prove the waist's headline behavior: a subscriber registered on the handle
receives the backend's `Tick` stream once playback starts — validating
`subscribe` + lazy `ensure_actor` + the forwarder together.

**Files:**
- Modify: `crates/soul-player-host/src/lib.rs` (test only — the code already exists from Task 3)

**Interfaces:**
- Consumes: `PlayerHandle::{offline, subscribe, open_folder}`, `Event::Tick`, `PlayState::Playing` (via `Tick.state`).
- Produces: nothing new (validation task).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module. (It imports `PlayState` through the backend; add
`use soul_player_backend::PlayState;` inside the test fn.)

```rust
    #[test]
    fn subscribe_receives_now_playing_tick() {
        use soul_player_backend::PlayState;
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.wav"), wav(48_000, 96_000)).unwrap();
        std::fs::write(dir.path().join("b.wav"), wav(48_000, 96_000)).unwrap();

        let h = PlayerHandle::offline();
        let (tx, rx) = mpsc::channel::<Event>();
        h.subscribe(move |ev| {
            let _ = tx.send(ev);
        });
        h.open_folder(dir.path()).expect("open_folder");

        // Wait (bounded) for a Tick showing track 0 playing.
        let deadline = Duration::from_secs(5);
        let mut saw_playing = false;
        while let Ok(ev) = rx.recv_timeout(deadline) {
            if let Event::Tick(t) = ev
                && t.now_playing_id == Some(0)
                && t.state == PlayState::Playing
            {
                saw_playing = true;
                break;
            }
        }
        assert!(saw_playing, "never observed track 0 playing via subscribe");
    }
```

- [ ] **Step 2: Run the test to verify it fails, then passes**

```bash
cargo test --manifest-path crates/soul-player-host/Cargo.toml subscribe_receives_now_playing_tick
```
Expected: PASS directly — the code path exists (Task 3 + 4). If it is flaky under
a loaded machine, the 5 s deadline already matches the backend's own actor test;
do not reduce it.

(If `PlayState` is not re-exported, this test's `use soul_player_backend::PlayState;`
still resolves because it imports straight from the backend crate — no `lib.rs`
re-export change required.)

- [ ] **Step 3: Full host suite + confirm nothing else regressed**

```bash
cargo test --manifest-path crates/soul-player-host/Cargo.toml
cargo test
```
Expected: host PASS (4 tests); root PASS (backend unchanged).

- [ ] **Step 4: Commit**

```bash
git add crates/soul-player-host/src/lib.rs
git commit -m "test(host): cover PlayerHandle event fan-out end-to-end"
```

---

## Final verification (after all tasks)

- [ ] Root workspace green + still headless:
```bash
cargo test
```
- [ ] Host workspace green:
```bash
cargo test --manifest-path crates/soul-player-host/Cargo.toml
```
- [ ] Tauri head still builds + tests from its new path:
```bash
cargo test --manifest-path app/tauri/src-tauri/Cargo.toml
```
- [ ] No stale names/paths:
```bash
grep -rn -E "soul_audio_service|soul-audio-service" --include=*.rs --include=*.toml . | grep -v -E "target/|/specs/|/spikes/" || echo "clean"
```

## What this plan intentionally does NOT do

- **No Rune head.** `app/rune` + the measured Tauri-vs-Rune bake-off are the
  follow-up plan, written once this foundation is green and Rune's pre-release API
  can be read from its `examples/hello` + `crates/rune-kit/README.md` (rather than
  guessed). See the spec's build-order steps 4–5.
- **No Tauri migration onto `soul-player-host`.** The Tauri head keeps its current
  `PlayerState`/IPC path (spec decision 4). Host has only test consumers until the
  Rune head arrives — that is deliberate; its unit tests prove it works standalone.
- **No workspace unification.** Four workspaces remain by design.
