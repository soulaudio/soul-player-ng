#!/usr/bin/env bash
# Verify a packaged macOS release before it is published. Usage:
#   verify-macos-bundle.sh <version> <arch: arm64|x86_64> <dmg> [app.tar.gz]
#
# The app under test is the one INSIDE the DMG — what users install (packaging the DMG
# also removes the standalone .app from target/). Checks: DMG layout (app + /Applications
# link), intact sealed (ad-hoc) signature, architecture, bundle identity and version,
# icon, the updater tarball's signature, and a launch smoke test (headless `--snap`
# render under the hardened runtime, in a throwaway HOME so no user config is touched).
set -euo pipefail

version="${1:?version}"
arch="${2:?arch}"
dmg="${3:?dmg}"
tarball="${4:-}"

fail() { echo "::error::$*" >&2; exit 1; }
ok() { echo "ok: $*"; }

pb() { /usr/libexec/PlistBuddy -c "Print :$1" "$app/Contents/Info.plist"; }

verify_sig() {
  codesign --verify --deep --strict --verbose=2 "$1" 2>&1 | sed 's/^/  /'
  local info
  info="$(codesign -dv "$1" 2>&1)"
  grep -q "Signature=adhoc" <<<"$info" || fail "$1: expected an ad-hoc signature"
  grep -q "Sealed Resources" <<<"$info" || fail "$1: resources not sealed (linker-only signature?)"
  grep -q "Info.plist entries" <<<"$info" || fail "$1: Info.plist not bound to the signature"
}

mnt="$(mktemp -d)"
# `diskutil image` (macOS 26+) replaces the deprecated `hdiutil attach`; the macOS 15
# hosted runners don't have it yet.
if diskutil image attach --help >/dev/null 2>&1; then
  diskutil image attach --readOnly --nobrowse --mountPoint "$mnt" "$dmg" >/dev/null
else
  hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dmg" >/dev/null
fi
trap 'diskutil eject "$mnt" >/dev/null || true' EXIT
app="$mnt/Soul Player.app"
[ -d "$app" ] || fail "DMG has no Soul Player.app"
[ "$(readlink "$mnt/Applications")" = "/Applications" ] || fail "DMG has no /Applications link"
ok "DMG layout"

verify_sig "$app"
ok "sealed ad-hoc signature"

exe="$app/Contents/MacOS/$(pb CFBundleExecutable)"
archs="$(lipo -archs "$exe")"
[ "$archs" = "$arch" ] || fail "binary architectures '$archs', expected '$arch'"
ok "architecture $archs"

[ "$(pb CFBundleIdentifier)" = "co.datadir.soulplayer" ] || fail "bundle id $(pb CFBundleIdentifier)"
[ "$(pb CFBundleShortVersionString)" = "$version" ] \
  || fail "bundle version $(pb CFBundleShortVersionString), expected $version"
icon="$(pb CFBundleIconFile)"
[ -f "$app/Contents/Resources/$icon" ] || [ -f "$app/Contents/Resources/$icon.icns" ] \
  || fail "icon $icon missing from Resources"
ok "identity co.datadir.soulplayer $version, icon $icon"

if [ -n "$tarball" ]; then
  x="$(mktemp -d)"
  tar -xzf "$tarball" -C "$x"
  verify_sig "$x/Soul Player.app"
  ok "updater tarball signature"
fi

# Launch smoke test: render one frame headlessly and exit.
home="$(mktemp -d)"
png="$home/smoke.png"
HOME="$home" "$exe" --snap "$png" --size 1000x640 >"$home/smoke.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
if kill -0 "$pid" 2>/dev/null; then
  kill -9 "$pid"; cat "$home/smoke.log"; fail "smoke launch did not finish within 120 s"
fi
wait "$pid" || { cat "$home/smoke.log"; fail "smoke launch exited non-zero"; }
[ -s "$png" ] || { cat "$home/smoke.log"; fail "smoke launch produced no frame"; }
ok "launches and renders ($(wc -c <"$png" | tr -d ' ') byte frame)"
