# Rune Head Migration — Implementation Plan (Phases 1–2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the native Rune head the one UI — bring `app/rune` onto current `main`, add a Settings page whose Sources section manages the host's watched folders, and delete the Tauri head.

**Architecture:** `app/rune` is a self-contained cargo workspace (`rune` git-pinned + `soul-player-host` path dep) whose `State` holds a `PlayerHandle`; views are `fn(&State, &Palette) -> El` and handlers call `s.host.*` directly (no IPC). The multi-source backend already exists in `soul-player-host::PlayerHandle` (`set_folders`/`add_folder`/`remove_folder`/`watched_folders` → `WatchUpdate{views, unavailable}`), so this is pure UI. Work happens on `feat/rune-head-migration` (branched from current `main` `ab24425`); the Rune tree is copied in from `spike/rune-head` rather than merged (the Tauri head is being deleted anyway).

**Tech Stack:** Rust 2024, Rune (anomalyco, git rev `07c369b…`, feature `native-file-dialog`), `soul-player-host`, `anyhow`. Headless test harness: `rune::Runtime::new(state, view, size, env)` + `rt.frame(dt)` + `rt.find("key")`.

## Global Constraints

- `#![forbid(unsafe_code)]` in every file.
- Commit messages: Conventional Commits; **NO** "Claude"/"Anthropic"/"AI"/"Generated with"/"Co-Authored-By"/any attribution (the `-ng` commit-msg hook rejects it).
- GitHub Actions is disabled for quota — **verify locally**, push `--no-verify` when already verified, merge via `gh pr merge --admin`.
- Disk is tight (~12 GB free). The Rune/wgpu first build is heavy; if a build fails on `no space on device`, `rm -rf` spare `target/` dirs (core/backend/host) before retrying — a full disk also masquerades as a headless-test timeout.
- Rune is pre-release (pinned `dev` rev, not on crates.io, no upstream PRs, IME not production-ready). The Sources UI must use the **native folder dialog** (`tx.open_folder`), never a text field.
- **Toolchain (found in Task 1):** `app/rune` requires **`nightly-2026-11-10`** (rustc `ac8310ba6`, which stabilized `if let` guards). Its `rust-toolchain.toml` pins it. **Gotcha:** rustup resolves `rust-toolchain.toml` from the **CWD**, NOT `--manifest-path` — so running `cargo --manifest-path app/rune/… ` from the repo root uses the ROOT's stable toolchain and fails on `rune`'s nightly features. Always build `app/rune` either with an explicit `cargo +nightly-2026-11-10 … --manifest-path app/rune/Cargo.toml`, or from within `app/rune` (CI: `working-directory: app/rune`, let the toml auto-install the nightly). Also: `PlayerHandle` now `#[derive(Clone)]` (host) — the head holds several Arc-handle clones.
- Do NOT plan Phase 3 (signed shipping) here — it is deferred per the spec.

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `app/rune/src/main.rs` | `State` + top-level `view` + event fold | Modify: add `Route`, 3 state fields, route switch in `view`, nav |
| `app/rune/src/views/mod.rs` | view module exports | Modify: `pub mod settings;` |
| `app/rune/src/views/settings.rs` | Settings page + Sources section | **Create** |
| `app/rune/src/views/sidebar.rs` | left sidebar | Modify: add Library/Settings nav; drop lone Open-Folder button |
| `app/rune/src/views/library.rs` | main track list + empty state | Modify: empty state routes to Settings |
| `app/tauri/`, `app/e2e/` | Tauri head + DOM e2e | **Delete** |
| `.github/workflows/ci.yml`, `security.yml`, `dependabot.yml` | CI | Modify: Tauri → Rune |
| `.github/workflows/release.yml`, `auto-release-on-version-bump.yml` | Tauri release pipeline | **Delete** (Phase 3 replaces) |

---

## Phase 1 — Promote Rune, remove Tauri

### Task 1: Bring in the Rune head and de-risk the build

**Files:**
- Create (copied from branch): `app/rune/**`
- Verify: `app/rune/Cargo.toml`, `app/rune/src/main.rs`

This task's deliverable is **a building, test-passing `app/rune` on this branch against current `main`'s `soul-player-host`** — the gate before any feature work. No code is written; it's a copy + build.

- [ ] **Step 1: Copy the Rune tree onto the branch**

```bash
cd D:/dev/soulaudio/soul-player-ng   # on feat/rune-head-migration
git checkout spike/rune-head -- app/rune
git checkout spike/rune-head -- docs/superpowers/plans/2026-11-09-rune-head.md docs/spikes/rune-head-dark.png docs/spikes/rune-head-light.png 2>/dev/null || true
git status --short | head
```
Expected: `app/rune/...` files staged/added; no `app/tauri` changes.

- [ ] **Step 2: Build the Rune head (the heavy wgpu compile)**

```bash
cargo build --manifest-path app/rune/Cargo.toml 2>&1 | tail -20
```
Expected: `Finished`. If it fails on the `soul-player-host` path dep (API drift), STOP — the host API is a superset of what the spike used (`open_folder`/`views`/`subscribe`/`send`/`art`), so a failure here is unexpected and must be understood, not patched over. If it fails on `no space on device`, free `target/` dirs and retry (see Global Constraints). Regenerate the lock if needed: `cargo update --manifest-path app/rune/Cargo.toml -p soul-player-host`.

- [ ] **Step 3: Run the existing headless tests**

```bash
cargo test --manifest-path app/rune/Cargo.toml 2>&1 | grep -E 'test result|FAILED|error'
```
Expected: `empty_library_shows_the_open_folder_button` passes (the `--ignored` real-backend test stays ignored). If the harness cannot create a GPU context headlessly on this machine, note it — tests may need a software adapter; do not proceed to Phase 2 until at least `rt.find` tests run.

- [ ] **Step 4: Commit**

```bash
git add app/rune docs/superpowers/plans/2026-11-09-rune-head.md docs/spikes/rune-head-*.png
git commit -m "chore(rune): bring the Rune head onto main (from spike/rune-head)"
```

### Task 2: Delete the Tauri head and rewire CI

**Files:**
- Delete: `app/tauri/`, `app/e2e/`
- Delete: `.github/workflows/release.yml`, `.github/workflows/auto-release-on-version-bump.yml`
- Modify: `.github/workflows/ci.yml`, `.github/workflows/security.yml`, `.github/dependabot.yml`

- [ ] **Step 1: Delete the Tauri head + Tauri release workflows**

```bash
git rm -r app/tauri app/e2e
git rm .github/workflows/release.yml .github/workflows/auto-release-on-version-bump.yml
```

- [ ] **Step 2: Rewire `ci.yml`** — remove the entire `frontend` job and every `app/tauri` step from the `rust` job; add an `app/rune` job. The Rune job needs Linux GPU/windowing deps and a software Vulkan adapter for headless tests:

```yaml
  rune:
    name: rune (fmt · clippy · test)
    runs-on: ubuntu-latest
    env:
      CARGO_TERM_COLOR: always
      CARGO_NET_GIT_FETCH_WITH_CLI: "true"
      CORE_REPO_TOKEN: ${{ secrets.CORE_REPO_TOKEN }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7
      - name: Authenticate the private soul-audio-core git dependency
        if: ${{ env.CORE_REPO_TOKEN != '' }}
        run: git config --global url."https://x-access-token:${CORE_REPO_TOKEN}@github.com/".insteadOf "https://github.com/"
      - name: GPU/windowing deps for Rune (wgpu + winit) + software Vulkan
        run: |
          sudo apt-get update
          sudo apt-get install -y libxkbcommon-x11-0 libwayland-dev libxkbcommon-dev \
            mesa-vulkan-drivers vulkan-tools libasound2-dev pkg-config xvfb
      - name: Set up Rust
        run: rustup component add rustfmt clippy
      - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2
        with:
          workspaces: app/rune
      - name: Format
        run: cargo fmt --manifest-path app/rune/Cargo.toml --all -- --check
      - name: Clippy
        run: cargo clippy --manifest-path app/rune/Cargo.toml --all-targets -- -D warnings
      - name: Test (headless, software Vulkan under Xvfb)
        run: xvfb-run -a cargo test --manifest-path app/rune/Cargo.toml
```
Update the `ci-success` job's `needs:` to `[rust, rune]` (drop `frontend`).

- [ ] **Step 3: Rewire `security.yml`** — repoint the two `app/tauri` references:

```yaml
# cargo audit step:
          cargo audit --file app/rune/Cargo.lock
# cargo deny step:
        working-directory: app/rune
        run: cargo deny --manifest-path app/rune/Cargo.toml check bans licenses sources
```
(Drop `--config deny.app.toml` if that config was Tauri-specific; use the repo default or add an `app/rune` deny config if the audit flags Rune's git dep — expected, since `rune` is a git source: add `[sources] allow-git = ["https://github.com/anomalyco/rune"]` to the deny config used here.)

- [ ] **Step 4: Rewire `dependabot.yml`** — delete the three `/app/tauri*` entries; add one:

```yaml
  - package-ecosystem: "cargo"
    directory: /app/rune
    schedule:
      interval: weekly
```

- [ ] **Step 5: Verify nothing else references Tauri, and the workspaces still build**

```bash
grep -rnE 'app/tauri|app/e2e|tauri' .github Cargo.toml scripts 2>/dev/null | grep -v node_modules || echo "clean"
cargo build --manifest-path app/rune/Cargo.toml 2>&1 | tail -1
cargo test 2>&1 | grep -E 'test result' | tail -2            # root backend workspace
cargo test --manifest-path crates/soul-player-host/Cargo.toml 2>&1 | grep -E 'test result' | tail -1
```
Expected: no stray Tauri references (or only harmless ones in docs); all three green.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(app): remove the Tauri head; Rune is the one UI"
```

---

## Phase 2 — Settings page + Sources

### Task 3: `State` gains routing + watched-folder fields

**Files:**
- Modify: `app/rune/src/main.rs`
- Test: `app/rune/src/main.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `enum Route { Library, Settings }` (derive `Clone, Copy, PartialEq`); `State.route: Route`, `State.watched_folders: Vec<PathBuf>`, `State.unavailable: Vec<PathBuf>`; `pub(crate) fn apply_watch(s: &mut State, up: WatchUpdate)`.

- [ ] **Step 1: Add the failing test** (in `main.rs` tests module; add `use std::path::PathBuf;` and `use soul_player_host::WatchUpdate;` to the test imports):

```rust
#[test]
fn new_state_starts_on_library_with_host_watched_folders() {
    let s = State::new(PlayerHandle::offline());
    assert_eq!(s.route, Route::Library);
    assert!(s.watched_folders.is_empty());
    assert!(s.unavailable.is_empty());
}

#[test]
fn apply_watch_folds_a_watchupdate_into_state() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.wav"), super::tests::wav(48_000, 96_000)).unwrap();
    let host = PlayerHandle::offline();
    let up = host.add_folder(dir.path()).unwrap();
    let mut s = State::new(host);
    apply_watch(&mut s, up);
    assert_eq!(s.watched_folders, vec![dir.path().to_path_buf()]);
    assert_eq!(s.library.len(), 1);
    assert!(s.unavailable.is_empty());
}
```
(Reuse the `wav` helper already present in the spike's `soul-player-host` tests pattern; if `main.rs` lacks one, copy the 20-line `wav(rate, frames)` writer from `crates/soul-player-host/src/lib.rs` into the test module.)

- [ ] **Step 2: Run — expect failure** (`Route`/fields/`apply_watch` undefined):

```bash
cargo test --manifest-path app/rune/Cargo.toml new_state_starts 2>&1 | grep -E 'error|cannot find'
```

- [ ] **Step 3: Implement** — add near the top of `main.rs`:

```rust
use std::path::PathBuf;
use soul_player_host::WatchUpdate;

#[derive(Clone, Copy, PartialEq)]
enum Route {
    Library,
    Settings,
}
```
Add three fields to `struct State` (after `art: ArtCache,` is fine):
```rust
    route: Route,
    watched_folders: Vec<PathBuf>,
    unavailable: Vec<PathBuf>,
```
In `State::new`, initialize them (the host already knows its watched set):
```rust
        let watched_folders = host.watched_folders();
        Self {
            host,
            library,
            // ...existing fields...
            route: Route::Library,
            watched_folders,
            unavailable: Vec::new(),
            art: ArtCache::new(),
        }
```
Add the fold helper (module level in `main.rs`):
```rust
/// Fold a `WatchUpdate` (from add/remove/set-folders) into UI state: refresh the
/// library + Sources list; playback is deliberately left untouched.
pub(crate) fn apply_watch(s: &mut State, up: WatchUpdate) {
    s.library = up.views;
    s.watched_folders = s.host.watched_folders();
    s.unavailable = up.unavailable;
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test --manifest-path app/rune/Cargo.toml new_state_starts apply_watch_folds 2>&1 | grep -E 'test result'
```
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add app/rune/src/main.rs
git commit -m "feat(rune): add Route + watched-folder state with apply_watch fold"
```

### Task 4: Settings page with the Sources section

**Files:**
- Create: `app/rune/src/views/settings.rs`
- Modify: `app/rune/src/views/mod.rs` (`pub mod settings;`)
- Test: `app/rune/src/main.rs` tests (headless, via the top-level `view`)

**Interfaces:**
- Consumes: `State{route, watched_folders, unavailable, host, library}`, `apply_watch`, `theme::Palette`, the `tx.open_folder(FileDialogOptions, cb)` pattern from `views/open_folder.rs`.
- Produces: `pub fn settings_page(s: &State, p: &Palette) -> El` keyed `settings`; a Sources section keyed `sources`; an add button keyed `add-source`; per-folder rows keyed `source-<basename>` with a remove control keyed `remove-<basename>`.

- [ ] **Step 1: Add failing headless tests** (in `main.rs` tests; mirror `empty_library_shows_the_open_folder_button`). Helper to build a Settings-routed state:

```rust
fn settings_state(folders: &[&std::path::Path], unavailable: &[&std::path::Path]) -> State {
    let host = PlayerHandle::offline();
    let paths: Vec<std::path::PathBuf> = folders.iter().map(|p| p.to_path_buf()).collect();
    if !paths.is_empty() {
        // populate the host's set so watched_folders() reflects them
        let _ = host.set_folders(&paths);
    }
    let mut s = State::new(host);
    s.route = Route::Settings;
    s.unavailable = unavailable.iter().map(|p| p.to_path_buf()).collect();
    s
}

#[test]
fn settings_route_shows_sources_section_and_add_button() {
    let env = Env { system_dark: true, scale: 2.0 };
    let mut rt = Runtime::new(settings_state(&[], &[]), view, (1100.0, 720.0), env);
    rt.frame(0.0);
    assert!(rt.find("sources").is_some(), "Settings shows a Sources section");
    assert!(rt.find("add-source").is_some(), "Sources offers Add Source");
}

#[test]
fn sources_list_renders_a_watched_folder_with_remove() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.wav"), tests::wav(48_000, 96_000)).unwrap();
    let env = Env { system_dark: true, scale: 2.0 };
    let mut rt = Runtime::new(settings_state(&[dir.path()], &[]), view, (1100.0, 720.0), env);
    rt.frame(0.0);
    let name = dir.path().file_name().unwrap().to_string_lossy().to_string();
    assert!(rt.find(&format!("source-{name}")).is_some(), "the watched folder renders");
    assert!(rt.find(&format!("remove-{name}")).is_some(), "with a remove control");
}

#[test]
fn unavailable_folder_shows_offline_badge() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.wav"), tests::wav(48_000, 96_000)).unwrap();
    let env = Env { system_dark: true, scale: 2.0 };
    // folder is watched AND currently unavailable
    let mut rt = Runtime::new(settings_state(&[dir.path()], &[dir.path()]), view, (1100.0, 720.0), env);
    rt.frame(0.0);
    let name = dir.path().file_name().unwrap().to_string_lossy().to_string();
    assert!(rt.find(&format!("offline-{name}")).is_some(), "an unavailable source shows the offline badge");
}
```

- [ ] **Step 2: Run — expect failure** (`view` still renders only the library; `settings` key absent):

```bash
cargo test --manifest-path app/rune/Cargo.toml settings_route sources_list unavailable_folder 2>&1 | grep -E 'FAILED|error|panicked'
```
(These compile only after `view` routes to `settings_page` — do Task 5's `view` switch together if the test won't compile; otherwise they fail at `rt.find("sources").is_none()`.)

- [ ] **Step 3: Create `app/rune/src/views/settings.rs`** (mirror `open_folder.rs`'s dialog + `library.rs`'s row idioms exactly):

```rust
//! Settings page. Its one section, "Sources", manages the host's watched-folder
//! set (soul_player_host::PlayerHandle::{add_folder, remove_folder,
//! watched_folders}); edits refresh the library + list but never disturb
//! playback. Add uses the native folder dialog (Rune IME is pre-release — no
//! text entry here).
#![forbid(unsafe_code)]

use rune::tok::type_;
use rune::{El, FileDialogOptions, ScrollSpec, Tx, col, el, row, text};

use crate::theme::Palette;
use crate::{State, apply_watch};

fn basename(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// `+ Add Source` — native folder dialog, then `host.add_folder` folded via `apply_watch`.
fn add_source_button(p: &Palette) -> El {
    el().key("add-source")
        .padding(12.0)
        .radius(8.0)
        .fill(p.primary)
        .center()
        .child(text("+ Add Source").color(p.bg))
        .on_press_tx(|_s: &mut State, tx: &mut Tx| {
            tx.open_folder(
                FileDialogOptions { title: "Add a music folder".into(), ..Default::default() },
                |s: &mut State, result, _tx| {
                    if let Ok(Some(path)) = result
                        && let Ok(up) = s.host.add_folder(&path)
                    {
                        apply_watch(s, up);
                    }
                },
            );
        })
}

fn source_row(s: &State, p: &Palette, index: usize) -> El {
    let folder = s.watched_folders[index].clone();
    let name = basename(&folder);
    let unavailable = s.unavailable.iter().any(|u| u == &folder);
    let remove_for = folder.clone();
    let mut r = row()
        .key(format!("source-{name}"))
        .gap(12.0)
        .padding(8.0)
        .child(text(name.clone()).color(p.fg).grow(1.0));
    if unavailable {
        r = r.child(
            text("offline")
                .key(format!("offline-{name}"))
                .color(p.text_secondary),
        );
    }
    r.child(
        el().key(format!("remove-{name}"))
            .padding(6.0)
            .radius(6.0)
            .fill(p.muted)
            .child(text("✕").color(p.text_secondary))
            .on_press(move |st: &mut State| {
                if let Ok(up) = st.host.remove_folder(&remove_for) {
                    apply_watch(st, up);
                }
            }),
    )
}

pub fn settings_page(s: &State, p: &Palette) -> El {
    let mut sources = col()
        .key("sources")
        .gap(2.0)
        .child(
            row()
                .padding(8.0)
                .child(text("Sources").font(type_::TITLE).color(p.primary).grow(1.0))
                .child(add_source_button(p)),
        );
    if s.watched_folders.is_empty() {
        sources = sources.child(
            text("No sources yet — add a folder to build your library.")
                .color(p.text_secondary)
                .padding(8.0),
        );
    } else {
        for i in 0..s.watched_folders.len() {
            sources = sources.child(source_row(s, p, i));
        }
    }
    col()
        .key("settings")
        .grow(1.0)
        .min_h(0.0)
        .child(text("Settings").font(type_::TITLE).color(p.fg).padding(16.0))
        .child(sources.scroll(ScrollSpec { stick_bottom: false }).grow(1.0).min_h(0.0))
}
```

- [ ] **Step 4: Export it** — `app/rune/src/views/mod.rs`: add `pub mod settings;`.

- [ ] **Step 5: Route `view` to it** (do here if not already done in Task 5): in `main.rs` `view`, replace the single library child with a route switch:

```rust
        .child(
            col().grow(1.0).min_h(0.0).child(match s.route {
                Route::Library => views::library::track_list(s, &p).grow(1.0),
                Route::Settings => views::settings::settings_page(s, &p).grow(1.0),
            }),
        )
```

- [ ] **Step 6: Run — expect pass**

```bash
cargo test --manifest-path app/rune/Cargo.toml settings_route sources_list unavailable_folder 2>&1 | grep -E 'test result'
cargo clippy --manifest-path app/rune/Cargo.toml --all-targets -- -D warnings 2>&1 | tail -1
```
Expected: 3 passed; clippy clean.

- [ ] **Step 7: Commit**

```bash
git add app/rune/src/views/settings.rs app/rune/src/views/mod.rs app/rune/src/main.rs
git commit -m "feat(rune): Settings page with a Sources section bound to the watched-folder contract"
```

### Task 5: Sidebar navigation between Library and Settings

**Files:**
- Modify: `app/rune/src/views/sidebar.rs`
- Test: `app/rune/src/main.rs` tests

**Interfaces:**
- Consumes: `State.route`, `settings_page`/`track_list` via `view`.
- Produces: sidebar nav items keyed `nav-library` and `nav-settings`, each `.on_press(|s| s.route = …)`.

- [ ] **Step 1: Add the failing test:**

```rust
#[test]
fn sidebar_has_library_and_settings_nav() {
    let env = Env { system_dark: true, scale: 2.0 };
    let mut rt = Runtime::new(State::new(PlayerHandle::offline()), view, (1100.0, 720.0), env);
    rt.frame(0.0);
    assert!(rt.find("nav-library").is_some());
    assert!(rt.find("nav-settings").is_some());
}
```

- [ ] **Step 2: Run — expect failure** (`nav-library` absent):

```bash
cargo test --manifest-path app/rune/Cargo.toml sidebar_has_library 2>&1 | grep -E 'FAILED|panicked'
```

- [ ] **Step 3: Implement** — in `sidebar.rs`, replace the lone Open-Folder button block (the `.child(el().px(16.0).py(8.0).child(open_folder_button("Open Folder", p)))`) with two nav items (highlight the active route):

```rust
        .child(nav_item("nav-library", "Tracks", s.route == crate::Route::Library, p))
        .child(nav_item("nav-settings", "Settings", s.route == crate::Route::Settings, p))
```
Add the helper + swap the `Tracks` title for the nav items (drop the now-unused `open_folder_button` import):
```rust
fn nav_item(key: &str, label: &str, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .px(16.0)
        .py(10.0)
        .child(text(label).color(if active { p.primary } else { p.text_secondary }))
        .on_press({
            let route = if key == "nav-settings" { crate::Route::Settings } else { crate::Route::Library };
            move |s: &mut State| s.route = route
        })
}
```
(`Route` must be visible to `sidebar.rs` — ensure `enum Route` in `main.rs` is `pub(crate)`.)

- [ ] **Step 4: Run — expect pass**

```bash
cargo test --manifest-path app/rune/Cargo.toml sidebar_has_library 2>&1 | grep -E 'test result'
```

- [ ] **Step 5: Commit**

```bash
git add app/rune/src/views/sidebar.rs app/rune/src/main.rs
git commit -m "feat(rune): sidebar nav between Library and Settings"
```

### Task 6: Retire the single-folder entry point; empty state routes to Settings

**Files:**
- Modify: `app/rune/src/views/library.rs`
- Test: `app/rune/src/main.rs` tests

**Interfaces:**
- Consumes: `State.route`.
- Produces: the empty-library state offers a `go-to-sources` control that sets `route = Settings` (replacing the old `open_folder_button`).

- [ ] **Step 1: Add the failing test:**

```rust
#[test]
fn empty_library_points_to_sources() {
    let env = Env { system_dark: true, scale: 2.0 };
    let mut rt = Runtime::new(State::new(PlayerHandle::offline()), view, (1100.0, 720.0), env);
    rt.frame(0.0);
    assert!(rt.find("go-to-sources").is_some(), "empty library routes the user to Sources");
    assert!(rt.find("open-folder").is_none(), "the single-folder Open Folder button is gone");
}
```

- [ ] **Step 2: Run — expect failure** (`open-folder` still present):

```bash
cargo test --manifest-path app/rune/Cargo.toml empty_library_points 2>&1 | grep -E 'FAILED|panicked'
```

- [ ] **Step 3: Implement** — in `library.rs` empty-state branch, replace `.child(open_folder_button("Open Folder", p))` and drop the `open_folder` import:

```rust
            .child(
                el().key("go-to-sources")
                    .padding(12.0)
                    .radius(8.0)
                    .fill(p.primary)
                    .center()
                    .child(text("Add a source").color(p.bg))
                    .on_press(|s: &mut State| s.route = crate::Route::Settings),
            );
```

- [ ] **Step 4: Run — expect pass + full suite + clippy**

```bash
cargo test --manifest-path app/rune/Cargo.toml 2>&1 | grep -E 'test result'
cargo clippy --manifest-path app/rune/Cargo.toml --all-targets -- -D warnings 2>&1 | tail -1
cargo fmt --manifest-path app/rune/Cargo.toml --all
```
Expected: all pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add app/rune/src/views/library.rs app/rune/src/main.rs
git commit -m "feat(rune): retire single-folder open; empty library routes to Sources"
```

---

## Phase 3 — deferred

Signed shipping + auto-update (`cargo-packager` + `cargo-packager-updater`, GitHub Releases, Authenticode/notarization) is specified in the design doc §Phase 3 and is **not planned here**. It begins once the Apple Developer + Windows signing accounts exist; unsigned local/dev builds (`cargo build --manifest-path app/rune/Cargo.toml`) suffice until then.

---

## Landing

After Task 6, verify the whole repo locally (`app/rune` tests + `cargo test` in the root backend workspace + `crates/soul-player-host`), push `--no-verify`, open a PR, and `gh pr merge --admin` (Actions disabled). The PR both removes Tauri and lands the Rune Settings/Sources UI.

## Self-Review notes

- **Spec coverage:** Phase 1 (promote/remove, CI rewire) → Tasks 1–2; Phase 2 (State, Settings page, Sources add/remove, unavailable badge, nav, retire single-open, playback-untouched) → Tasks 3–6; Phase 3 explicitly deferred. Covered.
- **Rune-API caveat:** `rune` is a pinned pre-release rev; exact builder/`Runtime`/`FileDialogOptions` signatures are mirrored from the spike's existing `open_folder.rs`/`library.rs`/`main.rs`. If a method name differs against the rev, match the existing head's usage (it compiles) rather than this plan's literal spelling. `on_press` vs `on_press_tx`: use `_tx` only when the handler needs `Tx` (the dialog); plain `on_press` otherwise — as the existing views do.
- **Type consistency:** `Route` (pub(crate)), `apply_watch(&mut State, WatchUpdate)`, keys `settings`/`sources`/`add-source`/`source-<name>`/`remove-<name>/`offline-<name>`/`nav-library`/`nav-settings`/`go-to-sources` are used identically across tasks.
