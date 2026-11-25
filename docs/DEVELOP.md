# Developing

Three workspaces, two toolchains:

| Workspace | Toolchain | Build |
|---|---|---|
| root (`crates/soul-player-backend`, `crates/soul-server`) | stable | `cargo build` |
| `crates/soul-player-host` | stable | `cargo build --manifest-path crates/soul-player-host/Cargo.toml` |
| `app` | **nightly**, pinned in `app/rust-toolchain.toml` | `cd app && cargo build --release` |

The Rune head needs nightly because `rune` uses unstable compiler features. The pin is in
the repo, so `rustup` picks it up automatically.

## Tests

```sh
cargo test                                                      # backend + server
cargo test --manifest-path crates/soul-player-host/Cargo.toml   # host
cd app && cargo test                                       # UI head (needs a GPU; see below)
```

## Linux build dependencies

Nothing in the code is Linux-specific, but the build needs system libraries the other two
platforms ship in their SDKs. On Debian/Ubuntu:

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
| `libxcb1-dev` | libX11's transport; the binary **links** `-lxcb`, so the runtime `libxcb1` alone is not enough |
| `mesa-vulkan-drivers` | a Vulkan ICD for `wgpu`; any hardware driver also does |

Running the Rune head's tests headlessly (what CI does) needs `xvfb` on top, and a
software adapter if the box has no GPU: `xvfb-run -a cargo test`.

## Nix / NixOS

`flake.nix` provides both toolchains and every native dependency above, so the whole thing
builds with no global installs:

```sh
nix develop            # stable pin (rust-toolchain.toml): backend + server + host
nix develop .#rune     # the pinned nightly + xvfb/vulkan-tools: app
```

Both shells read the pins straight from the repo's own `rust-toolchain.toml` files, so
they can't drift from what CI builds. (NixOS needs the flake rather than `rustup`:
rustup's downloaded toolchains aren't patched for the Nix dynamic linker.) The shell
leaves a system Vulkan driver alone when there is one and only falls back to Mesa's
Lavapipe when no hardware ICD exists.

## Desktop-session E2E (Linux)

`app/tests/linux_session_e2e.rs` launches the real binary and checks what it presents
to the desktop: the MPRIS session, its metadata and artwork, the transport controls a
media key drives, playback advancing at wall-clock rate, the window's `app_id`, and a
clean exit on SIGTERM. These are the failures that only exist at the session boundary,
where no unit test can reach.

They need a session bus and a display, so they are `#[ignore]`d by default:

```sh
cd app
dbus-run-session -- xvfb-run -a cargo test --test linux_session_e2e -- --ignored
```

Two caveats. The `app_id` test reads the compositor's window tree, so it needs a **sway**
session rather than Xvfb; elsewhere it prints a notice and skips, and CI's headless
Wayland job is what actually enforces it. And the suite needs a working audio sink: on a
machine without one, start a null sink first:

```sh
pulseaudio -D --exit-idle-time=-1 -n --load="module-null-sink sink_name=ci_sink" \
  --load="module-always-sink"
```

CI runs the whole thing inside headless sway; see the `rune (Wayland)` job.

## Screenshots

The UI head can render a named scene headlessly, which is how the README screenshot and
the UI reference shots are produced:

```sh
cd app
cargo build --release
./target/release/soul-player --snap out.png --size 1440x900 --scene grid-playing --theme dark
```

`grid-playing` is the one the README uses: a synthetic 20-album library (no files
needed) with playback running and a populated queue. `grid` is the same library at
rest. Point `SOUL_PLAYER_FOLDER` at a real library and use `--scene albums` to shoot
real covers. The scene table is in `app/src/main.rs`.
