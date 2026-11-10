# soul-player

Cross-platform **desktop** music player (Windows / macOS / Linux) with a native,
all-Rust UI — [Rune] (GPU-rendered, no webview) over the [`soul-audio-core`][core]
engine: a from-scratch `no_std` / no-alloc / `forbid(unsafe_code)` decode →
resample → playback library.

## Layout

| Path | Role |
|---|---|
| `crates/soul-player-backend` | Headless backend: Command/Event DTOs + the `AudioPlayer` actor over the engine. Sink-generic, device-free. |
| `crates/soul-player-host` | The `PlayerHandle` waist the UI binds to — picks the audio sink, owns the live `Library`, fans backend events out. |
| `app/rune` | The native [Rune] UI head over `soul-player-host`. Its own workspace, on a pinned nightly (see below). |

The audio engine, resampler, codecs, and the cpal sink come from
[`soul-audio-core`][core] as git dependencies; only the app-specific backend,
host, and UI live here.

## Develop

The Rune head is its own workspace and needs a **nightly** toolchain — `rune`
uses unstable compiler features — pinned in `app/rune/rust-toolchain.toml`.

```sh
cd app/rune
cargo build --release          # build the player
cargo packager --release       # package an installer (cargo install cargo-packager first)
```

The backend + host are stable-toolchain workspaces:

```sh
cargo test                                                      # backend (root workspace)
cargo test --manifest-path crates/soul-player-host/Cargo.toml   # host
```

See `app/rune/PACKAGING.md` for installers + auto-update.

## Status

Feature-complete on desktop: multi-source watched-folder library (add / remove /
rescan, sortable by artist / album / title / folder, persisted across launches),
gapless playback, seek, volume, cover art, light/dark themes, and every codec the
engine decodes. Mobile is out of scope — this is a desktop target.

[Rune]: https://github.com/anomalyco/rune
[core]: https://github.com/soulaudio/soul-audio-core
