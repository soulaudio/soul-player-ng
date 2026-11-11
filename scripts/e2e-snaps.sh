#!/usr/bin/env bash
# Visual e2e: render the real Rune UI head to PNGs for inspection.
#
# Rune's headless `--snap` renders the actual app offscreen (GPU, no visible
# window) — so this works both on a dev box and in CI (software Vulkan + Xvfb),
# and sidesteps the "GUI window is invisible to the agent" problem entirely. It
# drives the committed `--scene`s (see app/rune/src/main.rs) to capture each view.
#
# Inspect the PNGs by eye. A future step can diff them against committed
# references for visual-regression e2e in CI.
#
# Usage:  scripts/e2e-snaps.sh [OUT_DIR]      (default: app/rune/target/e2e-snaps)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/app/rune/target/e2e-snaps}"
mkdir -p "$out"

# The Rune head is a nested nightly workspace; its toolchain resolves from CWD.
( cd "$root/app/rune" && cargo build )
exe="$root/app/rune/target/debug/soul-player-rune"
[ -f "$exe.exe" ] && exe="$exe.exe"

# The app expects a native path; convert from the MSYS path on Windows.
data="$root/testdata"
data_native="$(cygpath -w "$data" 2>/dev/null || echo "$data")"

snap() { # snap <name> <scene|-> <theme>
  local name="$1" scene="$2" theme="$3"
  local args=(--snap "$out/$name.png" --size 1100x720 --scale 2 --theme "$theme")
  [ "$scene" != "-" ] && args+=(--scene "$scene")
  SOUL_PLAYER_FOLDER="$data_native" SOUL_PLAYER_NO_UPDATE=1 "$exe" "${args[@]}" >/dev/null 2>&1
  echo "  $out/$name.png"
}

echo "Rendering UI snapshots to $out:"
snap library-dark  -         dark
snap library-light -         light
snap settings      settings  dark
snap search        search    dark
snap no-match      no-match  dark
echo "Done. Open the PNGs to inspect the rendered UI."
