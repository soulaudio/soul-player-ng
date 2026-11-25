{
  description = "Soul Player: native all-Rust desktop music player";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Binary-distributed Rust toolchains, including the dated nightlies the Rune
    # UI head is pinned to. nixpkgs only carries the current stable, and rustup
    # can't self-manage toolchains on NixOS (downloaded binaries aren't patched
    # for the Nix dynamic linker), so the pins in rust-toolchain.toml are
    # resolved here instead.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
    in
    {
      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          inherit (pkgs) lib;

          # Both toolchains come straight from the repo's own pin files, so the
          # shell can never drift from what CI builds.
          stableToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          nightlyToolchain = pkgs.rust-bin.fromRustupToolchainFile ./app/rust-toolchain.toml;

          # Native libraries the Rust deps link against. The `-sys` build scripts
          # find these through pkg-config, which mkShell wires up from
          # buildInputs.
          # winit/wgpu dlopen() their windowing and graphics backends at runtime
          # rather than linking them, so the loader needs them on the path too.
          graphicsLibs = [
            pkgs.libxkbcommon       # winit: keyboard handling (+ its xcb bridge)
            pkgs.wayland            # winit: Wayland backend
            pkgs.libGL
            pkgs.vulkan-loader      # wgpu
            pkgs.libx11             # winit: X11 backend
            pkgs.libxcb             # libX11's transport, linked, not just dlopened
            pkgs.libxcursor
            pkgs.libxi
            pkgs.libxrandr
          ];

          nativeLibs = lib.optionals pkgs.stdenv.hostPlatform.isLinux ([
            pkgs.alsa-lib           # cpal's Linux backend (soul-audio-desktop)
            pkgs.dbus               # keyring -> dbus-secret-service -> libdbus-sys
            pkgs.openssl            # reqwest -> native-tls -> openssl-sys
          ] ++ graphicsLibs);

          runtimeLibs = lib.optionals pkgs.stdenv.hostPlatform.isLinux graphicsLibs;

          mkShell = { name, toolchain, extraPackages ? [ ] }:
            pkgs.mkShell {
              inherit name;

              packages = [ toolchain pkgs.pkg-config pkgs.git ] ++ extraPackages;
              buildInputs = nativeLibs;

              LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;

              # wgpu needs a Vulkan ICD. On NixOS the system drivers live under
              # /run/opengl-driver and the loader finds them on its own, leave
              # those alone so dev builds run on the real GPU. Only when there's
              # no hardware ICD (containers, CI, headless) fall back to Mesa's
              # Lavapipe software rasteriser, which is what CI's
              # mesa-vulkan-drivers provides.
              shellHook = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
                if [ -z "$VK_ICD_FILENAMES" ] \
                   && ! compgen -G "/run/opengl-driver/share/vulkan/icd.d/*.json" > /dev/null \
                   && ! compgen -G "/usr/share/vulkan/icd.d/*.json" > /dev/null; then
                  export VK_ICD_FILENAMES="${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.linuxArch}.json"
                  echo "no hardware Vulkan ICD found, using Mesa Lavapipe (software)"
                fi
              '';
            };
        in
        {
          # Stable workspaces: the root (backend + server) and the host.
          default = mkShell {
            name = "soul-player";
            toolchain = stableToolchain;
          };

          # The Rune UI head (`app/`): pinned nightly, plus the packaging and
          # headless-test tooling.
          rune = mkShell {
            # Developer-facing: this name is what the shell prompt shows, and it
            # has to tell the two shells apart (the stable one is "soul-player"),
            # so it keeps the head's internal name. Only PACKAGES drop "rune".
            name = "soul-player-rune";
            toolchain = nightlyToolchain;
            extraPackages = lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              pkgs.xvfb-run        # headless `cargo test` (CI does the same)
              pkgs.vulkan-tools    # vulkaninfo, for debugging adapter selection
              # `stage-linux.sh` unpacks the built .deb to stage the RPM's and
              # Flatpak's shared files. It falls back to `ar` + `tar` where
              # dpkg-deb is missing, but the dpkg path is the one CI takes, so
              # a local build should exercise the same one.
              pkgs.dpkg
            ];
          };
        });
    };
}
