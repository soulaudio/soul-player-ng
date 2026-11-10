# Rune Head Migration: Settings/Sources, Tauri Removal, and Shipping

**Date:** 2026-11-10
**Status:** Design — awaiting review before planning.

**Goal:** Make the native **Rune** head the one shipping UI for soul-player-ng: add a **Settings page with a Sources section** (manage multiple watched folders), **remove the Tauri head entirely**, and commit to a **cargo-packager** shipping/update stack (signed installers + auto-update) built as a later phase.

## Why

soul-audio is a Rust-purist project (`no_std`/`forbid(unsafe)` core) targeting a native e-ink Rust DAP. A **native Rust UI (Rune)** unifies the stack — core → `soul-player-host` → UI — with no TS/webview island, and binds the host contract *directly* (no IPC, no specta DTOs, no data-URL-over-IPC). The multi-source backend is already finished and tested in `soul-player-host::PlayerHandle` (`set_folders`/`add_folder`/`remove_folder`/`watched_folders` + `WatchUpdate{views, unavailable}`), so this feature is pure UI on a done backend — and dramatically smaller in Rune than it would be in Tauri.

## Decisions locked (this brainstorm)

- **Head:** Rune becomes the single UI; **Tauri is removed**, not kept on life-support.
- **Feature:** a **Settings page** (not a sidebar panel) containing a **Sources** section — add source, remove source, show unavailable roots.
- **Shipping stack:** **cargo-packager** + `cargo-packager-updater`, GitHub Releases as the backend (the "Tauri bundler without the webview" — same bundle formats, same asymmetric-key-signed updater model).
- **Shipping phase:** **deferred.** Land the UI + Tauri removal now with *unsigned dev builds*; build the signed installers + auto-update + OS code-signing CI as a separate follow-on once Apple/Azure signing accounts exist (~$220/yr).

## Current state (what we're changing)

- **Rune head** lives on branch `spike/rune-head` (PR #29, OPEN) as `app/rune/` — its own cargo workspace: `rune` (git, pinned rev `07c369b…`, feature `native-file-dialog`) + `soul-player-host` (path) + `anyhow`. A complete head: `main.rs` (State + `view` + `host.subscribe` event fold + key handling), `theme.rs`, `views/{library,open_folder,sidebar,player,queue,slider}.rs`, headless tests, screenshots, and a 587-line plan (`docs/superpowers/plans/2026-11-09-rune-head.md`).
- **Tauri head** is on `main` as `app/tauri/` (Svelte 5 + `src-tauri`) + `app/e2e/` (wdio/Edge WebDriver). It *duplicates* the host in `src-tauri/src/actor.rs::PlayerState` and does not depend on `soul-player-host`.
- The app heads are **separate workspaces**; the root workspace is only `crates/soul-player-backend`, so none of this touches core/host/backend.
- Tauri references to retire: `app/tauri/`, `app/e2e/`, and in `.github/`: `ci.yml` (rust + frontend jobs), `release.yml` (tauri-action), `auto-release-on-version-bump.yml` (reads `tauri.conf.json` version), `security.yml` (cargo-audit/deny on `app/tauri`), `dependabot.yml` (3 `app/tauri` entries).

---

## Phase 1 — Promote Rune, remove Tauri (the restructure)

Make `app/rune` the app head and delete Tauri. No new features; the Rune head keeps its existing single-folder `open_folder` behavior until Phase 2.

**Repo changes**
- Delete `app/tauri/` and `app/e2e/`. Keep `app/rune/` as the one head (optionally rename `app/rune` → `app/` later; keep `app/rune` for now to minimize churn).
- `.github/workflows/ci.yml`: delete the whole `frontend` job (npm/vite/svelte-check/vitest/playwright) and the `app/tauri` steps in `rust`; add an `app/rune` job: `fmt` + `clippy -D warnings` + `test`. On Linux runners the Rune build needs **GPU/windowing system deps** — install `libxkbcommon-x11` (+ the usual X/Vulkan libs) and run headless tests under **Lavapipe (CPU Vulkan) + Xvfb** (Rune's documented CI path). Keep the service + host jobs.
- `.github/workflows/release.yml` + `auto-release-on-version-bump.yml`: these are Tauri-action/`tauri.conf.json`-based — **remove them in Phase 1**; Phase 3 adds the cargo-packager release pipeline. (No signed releases between Phase 1 and Phase 3 — unsigned local/dev builds only.)
- `.github/workflows/security.yml`: repoint `cargo audit`/`cargo deny` from `app/tauri/src-tauri` to `app/rune`.
- `.github/dependabot.yml`: drop the 3 `app/tauri` entries; add one cargo entry for `app/rune`.
- Version source moves from `tauri.conf.json` to `app/rune/Cargo.toml`.

**Verification:** `app/rune` builds and its headless tests pass locally; `crates/soul-player-backend` and `crates/soul-player-host` unaffected. (Actions stays disabled per the quota memory; verify locally + `--admin` merge.)

**Risk owned here:** promoting Rune commits us to its pre-release reality — pinned `dev` rev (stable: `dev` is never rewritten), not on crates.io, **no upstream PRs** (file issues or fork; MIT so forking is legal), and **text-input/IME not production-ready**. Our Sources feature avoids text entry (it uses the native folder dialog), so this is tolerable now; any future text-heavy settings (search, rename) must re-weigh it.

---

## Phase 2 — Settings page + Sources (the feature)

A navigable **Settings** page in the Rune head whose first section manages the watched-folder **Sources**, bound directly to `PlayerHandle`.

**State (`main.rs`)** gains:
- `route: Route` where `enum Route { Library, Settings }` (default `Library`).
- `watched_folders: Vec<PathBuf>` — initialized from `host.watched_folders()`.
- `unavailable: Vec<PathBuf>` — from the most recent `WatchUpdate` (default empty).

**Navigation**
- The sidebar gains two nav entries (keyed for headless `find`): **Library** (→ `Route::Library`) and **Settings** (→ `Route::Settings`), each an `.on_press(|s| s.route = …)`.
- `view()` switches the **main area** on `s.route`: `Route::Library` → `views::library::track_list` (unchanged); `Route::Settings` → `views::settings::settings_page`. The sidebar (queue + now-playing panel) stays mounted across both.

**`views::settings::settings_page(s, p)`**
- A page with a title ("Settings") and a **Sources** section (the only section for now; the page is structured so more sections drop in later).
- **Sources section** (`key("sources")`):
  - Header "Sources" + a `+ Add Source` button (`key("add-source")`). Reuses the existing `open_folder.rs` dialog pattern: `.on_press_tx(|_s, tx| tx.open_folder(opts, |s, result, _| { if let Ok(Some(path)) = result { if let Ok(up) = s.host.add_folder(&path) { apply_watch(s, up); } } }))`.
  - One `source_row` per `s.watched_folders` entry (`key(format!("source-{basename}"))`): the folder's basename (full path available as secondary text), a muted **"offline"** badge when the entry is in `s.unavailable`, and a remove **✕** (`key(format!("remove-{basename}"))`) → `.on_press(|s| { if let Ok(up) = s.host.remove_folder(&folder) { apply_watch(s, up); } })` with `folder` captured by the closure.
  - Empty state (no watched folders): "No sources yet — add a folder to build your library."
- Shared helper `apply_watch(s, up: WatchUpdate)`: `s.library = up.views; s.watched_folders = s.host.watched_folders(); s.unavailable = up.unavailable;`.

**Old single-folder entry points**
- The sidebar's lone "Open Folder" button and `library.rs`'s empty-state button are replaced by routing to **Settings → Sources** (or a direct "Add a source…" that runs the same add-folder dialog). The watched-folder *set* is the library; `open_folder` (single, replace, autoplay) is retired from the UI (the method may remain for `autoload_from_env`/tests).

**Playback on source edits** (host-contract behavior, re-affirmed): add/remove **refresh the library + Sources list and do not touch playback or the queue** ("managing what's watched ≠ what's playing"). The user clicks a track to (re)queue against the current library.
- *Documented limitation / follow-up:* editing sources mid-playback leaves the actor's queue context pre-rebake, so auto-advance after the current track can land on the wrong track until the user clicks. A later follow-up can re-queue preserving the current track.

**Testing** (headless, following the existing head's proven harness — `Runtime::new(state, view, size, env)` + `rt.frame(..)` + `rt.find("key")`; if the pinned rev also exposes `Runtime::headless`/`render_to_png`, prefer it):
- `settings_route_shows_sources_section` — State at `Route::Settings` → `rt.find("sources")` present.
- `empty_sources_shows_add_button` — no watched folders → `rt.find("add-source")`, empty-state text.
- `sources_list_renders_watched_folders` — State with two watched folders → both `source-<name>` rows found.
- `unavailable_folder_shows_offline_badge` — a folder in `unavailable` renders the badge.
- `nav_switches_between_library_and_settings` — toggling `route` swaps the main area (library list vs sources).
- Integration test: an offline `PlayerHandle` over temp dirs; drive `host.add_folder`/`remove_folder` and assert `State` folds the returned `WatchUpdate` (library + watched_folders + unavailable). (The host's own add/remove/unavailable behavior is already covered by its 15 tests — this verifies the view-state wiring, not the backend.)

---

## Phase 3 — Signed shipping + auto-update (deferred)

The decided plan, implemented once signing accounts exist. Until then, `cargo-packager` can emit **unsigned** bundles for dev distribution (SmartScreen/Gatekeeper "unknown publisher" nags, otherwise functional).

**Bundling** — `cargo-packager` (CrabNebula; Tauri's bundler extracted) over the `soul-player-rune` binary, configured via `Packager.toml`/`Cargo.toml` metadata:
- Windows: NSIS setup + WiX **MSI** (Start-menu shortcut).
- macOS: `.app` + `.dmg`.
- Linux: `.deb` + **AppImage**.

**Auto-update** — `cargo-packager-updater`:
- `signer generate` → keypair; **public key embedded in-app**, private key (CI secret) signs each package; the app **verifies the signature before applying** (the trust model we already have under Tauri — do not regress to HTTPS-only trust).
- Backend: **GitHub Releases** as a static endpoint (`{{target}}/{{arch}}/{{current_version}}` templating; multiple fallback endpoints allowed). No server to run. (CrabNebula Cloud is an optional later add for staged rollouts/analytics.)
- Full-download updates (no deltas). *If* download size becomes a concern, **Velopack** (automatic zstd deltas, multi-backend) is the fallback option — at the cost of a .NET SDK build dependency and a younger Rust binding.

**OS code-signing** (framework-independent; the real recurring cost):
- **Windows (Authenticode):** prefer **Azure Trusted Signing** (~$120/yr, no hardware token, signs from CI; cargo-packager/Velopack support it) — *eligibility:* US/Canada org with ≥3 years legal history (or individual public preview, US/Canada). If ineligible → a traditional **OV cert on a hardware token** (~$150–300/yr, harder CI). EV is not worth the premium (no instant SmartScreen reputation advantage anymore).
- **macOS:** Apple Developer **$99/yr** + **Developer ID Application** cert + **hardened runtime** + `notarytool` **notarize** + **staple**. cargo-packager orchestrates codesign→notarize→staple in CI.
- **Linux:** no OS signing required (optional GPG-signed `.deb`/AppImage).
- **First-year cash ≈ $220/yr** (Apple $99 + Azure ~$120) + a weekend of CI wiring; everything else (tools, GitHub Releases backend, Linux) is free.

**Release workflow** (replaces the deleted `release.yml`): tag-triggered (`v*`) matrix build (Win x64, macOS arm64+x64, Linux x64) → `cargo packager` bundle → OS-sign → updater-sign → publish to a draft GitHub Release + the updater manifest; `auto-release-on-version-bump` reads the version from `app/rune/Cargo.toml`.

---

## Testing strategy (whole migration)

- **Backend untouched:** `soul-player-host` (15 tests, incl. the multi-source contract) and `soul-player-backend` need no changes; keep them green.
- **Rune head:** headless view/snapshot tests (above) + the existing event-fold tests; run under Lavapipe/Xvfb in CI once Actions is re-enabled, locally otherwise.
- **No DOM e2e:** the wdio/Edge WebDriver suite is deleted with Tauri; Rune's in-process headless harness replaces it (deterministic, no browser driver — a net simplification given the recurring Tauri e2e flakiness, e.g. the `folder_switch` Tick-wait timeout).
- **Shipping (Phase 3):** smoke-test an unsigned bundle per OS first; validate the signed update flow end-to-end (install vN, release vN+1, in-app update applies + verifies signature) before announcing auto-update.

## Risks & non-goals

**Risks**
- **Rune is pre-release:** pinned `dev` rev, no releases/crates.io, no upstream PRs, incomplete IME/a11y. Mitigation: the clean `PlayerHandle` boundary makes the UI low-regret — a renderer swap (GPUI/Slint/Xilem) wouldn't touch core/host. Revisit if Rune stalls.
- **GPU floor:** Rune needs working DX12/Vulkan (vs Tauri's WebView2). Fine for desktop; note it for support.
- **Single maintainer dependency risk** (Rune *and* cargo-packager are each effectively one-org projects) — both MIT/forkable; GitHub Releases backend avoids lock-in to any hosted service.

**Non-goals (explicit)**
- Persisting the watched-folder set across launches (needs a small config store — follow-up).
- Filesystem watching / live library refresh.
- Rune web/mobile targets.
- Delta updates (Velopack path noted as the fallback if needed).
- The re-queue-preserving-current-track refinement.
- Keeping Tauri in any form.
