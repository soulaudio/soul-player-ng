# soul-player

Cross-platform **desktop** music player (Windows / macOS / Linux) built with
[Tauri 2] + Svelte 5, on top of the [`soul-audio-core`][core] engine — the
from-scratch `no_std` / no-alloc / `forbid(unsafe_code)` decode → resample →
playback library.

## Layout

| Path | Role |
|---|---|
| `crates/soul-player-backend` | Headless backend service: app Command/Event DTOs + the `AudioPlayer` actor over the engine. Sink-generic, device-free. |
| `app/tauri` | Tauri 2 shell + Svelte 5 UI. `src-tauri` wires the cpal sink and the service; the webview renders the player. |

The audio engine, resampler, codecs, and the cpal sink come from
[`soul-audio-core`][core] as git dependencies; only the app-specific backend
(`soul-player-backend`) and the UI live here.

## Develop

```sh
cd app/tauri
npm install
npm run tauri dev      # run the player
npm run tauri build    # package an installer
```

## Status

The player is feature-complete on desktop (library scan, gapless playback, seek,
volume, cover art, every codec the engine decodes). Mobile is explicitly out of
scope — this is a desktop target.

[Tauri 2]: https://tauri.app
[core]: https://github.com/soulaudio/soul-audio-core
