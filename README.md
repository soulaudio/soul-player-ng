# soul-player

Cross-platform **desktop** music player (Windows / macOS / Linux) with a native,
all-Rust UI — [Rune] (GPU-rendered, no webview) over the [`soul-audio-core`][core]
engine: a from-scratch `no_std` / no-alloc / `forbid(unsafe_code)` decode →
resample → playback library.

## Install

Downloads are on the [releases page](https://github.com/soulaudio/soul-player-ng/releases/latest).

**Linux** — four package formats, for x86_64 and aarch64:

| | | updates via |
|---|---|---|
| `.deb` | Debian 12+, Ubuntu 22.04+ | `apt upgrade` |
| `.rpm` | Fedora, openSUSE, RHEL | `dnf upgrade` |
| `.AppImage` | anything with glibc 2.35+ | the app itself |
| Flatpak | anything with Flatpak | `flatpak update` |

The glibc floor is **2.35**, verified by installing and running on Ubuntu 22.04
(2.35), Debian 12 (2.36) and Ubuntu 24.04 (2.39). The binary also references two
glibc 2.39 symbols (`pidfd_spawnp`, `pidfd_getpid`, from Rust's process
spawning), but they are *weak* — glibc falls back to the older path when they
are absent, which is why older releases work.

```sh
sudo apt install ./soul-player-rune_*_amd64.deb     # Debian / Ubuntu (arm64 for ARM)
sudo dnf install ./soul-player-*.x86_64.rpm         # Fedora (aarch64 for ARM)
chmod +x Soul-Player*.AppImage && ./Soul-Player*.AppImage
```

Only the AppImage updates itself. The others are owned by their package
manager, so the app does not offer an in-app update there — it would have to
overwrite a root-owned file, which cannot work.

Bit-perfect exclusive output needs direct access to the sound card. Under
Flatpak that is the `--device=all` permission; revoking it in Flatseal leaves
shared playback working and only disables exclusive mode.

**Windows** — the `-setup.exe` installer. **macOS** — the `.dmg`.

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

### Linux

Nothing here is Linux-specific in the code, but the build needs system libraries
the other two platforms ship in their SDKs. On Debian/Ubuntu:

```sh
sudo apt-get install -y pkg-config \
  libasound2-dev libdbus-1-dev libssl-dev \
  libxkbcommon-dev libwayland-dev libxcb1-dev mesa-vulkan-drivers
```

| Package | Pulled in by |
|---|---|
| `libasound2-dev` | `cpal` → ALSA, the Linux audio sink |
| `libdbus-1-dev` | `keyring` → Secret Service (remote-source tokens) |
| `libssl-dev` | `reqwest` → `native-tls` → `openssl-sys` |
| `libxkbcommon-dev`, `libwayland-dev` | `winit` (Rune head only) |
| `libxcb1-dev` | libX11's transport — the binary **links** `-lxcb`, so the runtime `libxcb1` alone is not enough |
| `mesa-vulkan-drivers` | a Vulkan ICD for `wgpu` — any hardware driver also does |

Running the Rune head's tests headlessly (what CI does) needs `xvfb` on top, and
a software adapter if the box has no GPU: `xvfb-run -a cargo test`.

#### Desktop-session E2E

`app/rune/tests/linux_session_e2e.rs` launches the real binary and checks what it
presents to the desktop — the MPRIS session, its metadata and artwork, the
transport controls a media key drives, playback advancing at wall-clock rate, the
window's `app_id`, and a clean exit on SIGTERM. These are the failures that only
exist at the session boundary, where no unit test can reach.

They need a session bus and a display, so they are `#[ignore]`d by default:

```sh
cd app/rune
dbus-run-session -- xvfb-run -a cargo test --test linux_session_e2e -- --ignored
```

Two caveats. The `app_id` test reads the compositor's window tree, so it needs a
**sway** session rather than Xvfb; elsewhere it prints a notice and skips, and
CI's headless Wayland job is what actually enforces it. And the suite needs a
working audio sink: on a machine without one, start a null sink first —

```sh
pulseaudio -D --exit-idle-time=-1 -n --load="module-null-sink sink_name=ci_sink" \
  --load="module-always-sink"
```

CI runs the whole thing inside headless sway; see the `rune (Wayland)` job.

#### Nix / NixOS

`flake.nix` provides both toolchains and every native dependency above, so the
whole thing builds with no global installs:

```sh
nix develop            # stable pin (rust-toolchain.toml) — backend + host
nix develop .#rune     # the pinned nightly + xvfb/vulkan-tools — app/rune
```

Both shells read the pins straight from the repo's own `rust-toolchain.toml`
files, so they can't drift from what CI builds. (NixOS needs the flake rather
than `rustup`: rustup's downloaded toolchains aren't patched for the Nix dynamic
linker.) The shell leaves a system Vulkan driver alone when there is one and
only falls back to Mesa's Lavapipe when no hardware ICD exists.

## Status

Feature-complete on desktop: multi-source watched-folder library (add / remove /
rescan, sortable by artist / album / title / folder, persisted across launches),
gapless playback, seek, volume, cover art, light/dark themes, and every codec the
engine decodes. Mobile is out of scope — this is a desktop target.

## License

Licensed under the **[Apache License 2.0](LICENSE)** — fully open source, free to
use, modify, and redistribute, including in commercial and closed-source
products. Copyright © 2026 datadir s. r. o. (Sebastián Stupák). See
[LICENSE](LICENSE) and [NOTICE](NOTICE).

### Why open source

We ship this in the open because we can get things wrong, and we'd rather you
could read it, fix it, and build on it than take our word for it. The whole point
is to let the community build on this effort instead of reinventing it. Fork it,
learn from it, make something better.

### Credit — a request, not a requirement

If you build something on Soul Audio, we'd genuinely love a visible mention —
drop this anywhere users can see it (an About / Credits screen, your docs, a
footer):

> Powered by **[Soul Audio](https://github.com/soulaudio)**

It's a courtesy we appreciate, not a license condition. The name "Soul Audio" and
the logo are trademarks of datadir s. r. o. — please use them to credit us, not
in a way that implies we endorse your product.

[Rune]: https://github.com/anomalyco/rune
[core]: https://github.com/soulaudio/soul-audio-core
