# Spike: Rune vs Tauri for soul-player

**Date:** 2026-11-08
**Branch:** `spike/rune-vs-tauri`
**Question:** Would [anomalyco/rune](https://github.com/anomalyco/rune) be a better UI foundation than Tauri 2?

## Verdict

**No — do not migrate now.** Rune is interesting and philosophically aligned
(all-Rust, webview-free), and our architecture would make such a swap unusually
cheap later. But Rune is pre-release, has no stable releases, ships no crates.io
package, and is explicitly *"written for agents, not you"* — it refuses human
pull requests. Betting a feature-complete, shipping desktop product on it today
would be reckless. Keep Tauri; put Rune on a watch list.

## What Rune is

- Native **GPU-rendered** Rust UI framework (macOS / Windows / Linux / web). Its
  own renderer — **no webview, no HTML/CSS/JS**.
- Built on solid Rust-GUI primitives: `wgpu` (GPU), `Taffy` (flex/grid/block
  layout), `Parley`/`HarfRust`/`skrifa`/`fontique` (text), `AccessKit` (a11y),
  plus its own spring-animation system.
- Declarative **Rust builder API**: `col()`, `el()`, `text()`, chained
  `.size()`, `.fill()`, `.on_press(|s: &mut State| …)`. State is a struct passed
  to a view fn. (No DSL/markup — it's Rust code.)
- Headless mode is the headline feature: render to PNG, dump the element tree,
  drive scripted input — aimed at AI coding agents.
- By **Anomaly** (team behind OpenCode). MIT.

### Maturity / governance red flags

- **Pre-release.** Instruction is literally "pin a rev of `dev`" — no semver, no
  tagged releases, not on crates.io.
- **"We don't take pull requests."** Contribution model is agent-driven issues.
  For a dependency under a product we ship, that is a serious supply-chain and
  longevity risk — we'd be pinned to one org's dev branch with no community
  fork-ability in practice.
- No published benchmarks of its own. (The widely-cited 76%→17% CPU /
  3.7GiB→440MiB numbers are **GPUI + a screen recorder (Cap)**, not Rune, and not
  a music player — a mostly-idle audio app sees far smaller webview savings.)

## Why the question is reasonable for *this* repo

Our layering already isolates the UI, which is exactly what a renderer swap
needs:

| Layer | Crate / dir | Swappable? |
|---|---|---|
| Engine (decode→resample→playback, `no_std`, `forbid(unsafe)`) | `soul-audio-core` (git dep) | Keep as-is |
| Headless backend actor — Command/Event **DTOs**, sink-generic, device-free | `crates/soul-audio-service` (`types.rs`, `driver.rs`, `actor.rs`) | **Keep as-is** |
| Shell: wires cpal sink + service | `app/src-tauri` | Replaced |
| UI (webview) | `app/` Svelte 5 + Vite | Rewritten |

Because the service is already a transport-agnostic actor over Command/Event
DTOs, a native-Rust UI could sit directly on it — arguably *more* naturally than
the current IPC-to-webview bridge. The engine and all audio logic are untouched
by any UI choice. This is the one genuinely attractive angle.

## Cost / risk of switching today

- **Rewrite the entire feature-complete UI** (library scan, gapless playback,
  seek, volume, cover art) from Svelte 5 into Rune's Rust builder API.
- **Throw away the Tauri ecosystem we already depend on:** configured updater,
  NSIS + macOS installers/signing, WebView2 bootstrapper, plugin ecosystem.
- **Invalidate the e2e suite** — `app/e2e` is DOM-based wdio/Edge WebDriver;
  there is no DOM in Rune. (Rune's element-tree + headless PNG is a plausible
  replacement harness, but it's a from-scratch rewrite.)
- Pin production to an unversioned `dev` rev of a no-PR project.

## Benefits we'd actually get

- Drop the WebView2/WebKitGTK process → lower idle memory; one renderer across
  all OSes (kills the WebKitGTK-on-Linux inconsistency that bites Tauri apps).
- Single-language (all-Rust) stack — aligns with the repo's Rust-purist stance
  (`forbid(unsafe)`, `no_std` core).
- GPU-accelerated animation/springs.

For a music player these are real but modest; the webview tax is much lower here
than for a video/screen-recording app.

## If we ever go webview-free, Rune isn't the only option

More mature native-Rust peers to weigh first: **GPUI** (powers Zed), **Slint**,
**Xilem** (linebender), **Dioxus** (native renderer), **Floem**, **egui**. Most
have releases, semver, and open contribution — all things Rune currently lacks.

## Recommendation

1. **Stay on Tauri 2 + Svelte 5.** The product is feature-complete; the upside
   doesn't justify a full UI rewrite onto pre-release infra.
2. **Keep the service boundary clean** (it already is) so a native UI is a
   low-regret future option regardless of which framework wins.
3. **Watch Rune** for: a tagged/crates.io release, an open contribution model,
   and real published benchmarks. Revisit if all three land — and benchmark it
   against GPUI/Slint/Xilem at that point rather than adopting by default.
