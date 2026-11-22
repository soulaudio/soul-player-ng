#!/usr/bin/env bash
# macOS e2e: Finder-style "open documents" reach the REAL bundled app.
#
# `open -a "Soul Player" file` sends the same 'aevt'/'odoc' Apple event as a double-click,
# "Open With" or a drop on the Dock icon. Asserted through the system Now Playing state
# (`media-control`), in an isolated $HOME so real settings are never touched:
#   1. a file already in the library, opened while the app runs, plays;
#   2. a file from an unknown folder: the folder becomes a source and the file plays;
#   3. a file that launches the app plays.
#
# Usage: scripts/e2e-open-files-macos.sh [path/to/Soul Player.app]
#        (build it with: cd app/rune && cargo build --release && cargo packager --release -f app —
#         the packager only wraps the existing binary, it does not rebuild it)
# Needs: brew install media-control. Quits any running Soul Player first.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
app="${1:-$root/app/rune/target/release/Soul Player.app}"
command -v media-control >/dev/null || { echo "need media-control (brew install media-control)"; exit 2; }
[ -d "$app" ] || { echo "no bundle at $app"; exit 2; }

work="$(mktemp -d)"
quit_app() { pkill -TERM -f "Soul Player.app/Contents/MacOS/" 2>/dev/null || true; for _ in $(seq 1 40); do pgrep -f "Soul Player.app/Contents/MacOS/" >/dev/null || return 0; sleep 0.25; done; }
cleanup() { quit_app; rm -rf "$work"; }
trap cleanup EXIT
quit_app

# Silent tagged WAVs: one in the configured library, one in a folder the app doesn't know.
lib="$work/library"; other="$work/elsewhere"; home="$work/home"
mkdir -p "$lib" "$other" "$home/Library/Application Support/soul-player"
python3 - "$lib/in-library.wav" "Open E2E Library" "$other/not-yet.wav" "Open E2E Elsewhere" <<'EOF'
import struct, sys
def wav(path, title, secs=60, rate=44100):
    data = b'\0' * (secs * rate * 4)
    fmt = b'fmt ' + struct.pack('<IHHIIHH', 16, 1, 2, rate, rate * 4, 4, 16)
    body = b'INFO'
    for k, v in (('INAM', title), ('IART', 'Soul E2E'), ('IPRD', 'Open Files')):
        b = v.encode() + b'\0'
        b += b'\0' * (len(b) % 2)
        body += k.encode() + struct.pack('<I', len(v) + 1) + b
    lst = b'LIST' + struct.pack('<I', len(body)) + body
    riff = b'WAVE' + fmt + lst + b'data' + struct.pack('<I', len(data)) + data
    open(path, 'wb').write(b'RIFF' + struct.pack('<I', len(riff)) + riff)
a = sys.argv[1:]
for i in range(0, len(a), 2):
    wav(a[i], a[i + 1])
EOF
printf 'sources = ["%s"]\n' "$lib" > "$home/Library/Application Support/soul-player/config.toml"

field() { media-control get 2>/dev/null | python3 -c "import json,sys; d=json.load(sys.stdin) or {}; print(d.get('$1') or '')" 2>/dev/null || true; }
fail() { echo "FAIL: $*"; echo "--- app log:"; tail -25 "$home/Library/Logs/Soul Player/"*.log 2>/dev/null || true; exit 1; }
wait_title() {
  for _ in $(seq 1 120); do [ "$(field title)" = "$1" ] && return 0; sleep 0.25; done
  fail "now playing title: expected '$1', got '$(field title)'"
}
ok() { echo "  ok  $*"; }
launch() { open --env HOME="$home" -a "$app" "$@"; }   # no -n: later `open -a` must route to it

echo "== open-files e2e"
launch
for _ in $(seq 1 80); do pgrep -f "Soul Player.app/Contents/MacOS/" >/dev/null && break; sleep 0.25; done
# LaunchServices resolves by bundle id: with another copy installed (e.g. /Applications) it may
# run THAT one. Refuse to test the wrong binary.
running="$(ps -o command= -p "$(pgrep -f "Soul Player.app/Contents/MacOS/" | head -1)")"
case "$running" in
  "$app/Contents/MacOS/"*) ;;
  *) fail "LaunchServices started '$running', not the bundle under test ($app) — install this build there or remove the other copy" ;;
esac
sleep 3   # first frame + restored-library scan

open -a "$app" "$lib/in-library.wav";  wait_title "Open E2E Library";   ok "library file opened while running plays"
open -a "$app" "$other/not-yet.wav";   wait_title "Open E2E Elsewhere"; ok "unknown-folder file plays"
grep -q "$other" "$home/Library/Application Support/soul-player/config.toml" \
  || fail "the unknown file's folder was not added as a source"
ok "its folder became a source"

quit_app
launch "$lib/in-library.wav";          wait_title "Open E2E Library";   ok "a file that launches the app plays"
echo "PASS"
