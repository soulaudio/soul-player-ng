#!/usr/bin/env bash
# macOS e2e: the REAL app ↔ the system "Now Playing" service (Control Center widget,
# lock screen, media keys, AirPods controls all read/drive the same MediaRemote state).
#
# Launches the release app on a folder of SILENT tagged tracks (with a folder cover),
# autoplaying via the e2e-only SOUL_PLAYER_AUTOPLAY hook, then uses `media-control`
# (ungive/mediaremote-adapter — works on macOS 15.4+, where direct MediaRemote reads are
# entitlement-gated) to assert what macOS sees and to send remote commands exactly as a
# media key / AirPods tap would.
#
# Runs in an isolated $HOME (config, cache, logs), so it never touches real settings.
# EXCLUSIVE=1 also runs it with hog-mode exclusive output on (proves exclusive output
# does not knock the app out of Now Playing).
#
# Usage:  scripts/e2e-now-playing-macos.sh            (needs: brew install media-control)
#         EXCLUSIVE=1 scripts/e2e-now-playing-macos.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
command -v media-control >/dev/null || { echo "need media-control (brew install media-control)"; exit 2; }

work="$(mktemp -d)"
app_pid=""
cleanup() {
  [ -n "$app_pid" ] && kill "$app_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

# ---- fixture: two silent tagged WAVs (44.1k + 96k, so exclusive switches rate) + cover
music="$work/music"
mkdir -p "$music"
python3 - "$music" <<'EOF'
import os, struct, sys
d = sys.argv[1]
def info(tags):
    body = b'INFO'
    for k, v in tags:
        b = v.encode() + b'\0'
        if len(b) % 2: b += b'\0'
        body += k.encode() + struct.pack('<I', len(v) + 1) + b
    return b'LIST' + struct.pack('<I', len(body)) + body
def wav(name, secs, rate, title):
    data = b'\0' * (int(secs * rate) * 4)
    fmt = b'fmt ' + struct.pack('<IHHIIHH', 16, 1, 2, rate, rate * 4, 4, 16)
    lst = info([('INAM', title), ('IART', 'Soul E2E Artist'), ('IPRD', 'Now Playing Album')])
    riff = b'WAVE' + fmt + lst + b'data' + struct.pack('<I', len(data)) + data
    open(os.path.join(d, name), 'wb').write(b'RIFF' + struct.pack('<I', len(riff)) + riff)
wav('01 - First.wav', 60, 44100, 'Soul NP First')
wav('02 - Second.wav', 60, 96000, 'Soul NP Second')
EOF
cp "$root/app/rune/icons/128x128.png" "$music/cover.png"

home="$work/home"
mkdir -p "$home/Library/Application Support/soul-player"
if [ "${EXCLUSIVE:-0}" = "1" ]; then
  printf 1 > "$home/Library/Application Support/soul-player/output_exclusive.txt"
fi

( cd "$root/app/rune" && cargo build --release ${CARGO_EXTRA_ARGS:-} )
HOME="$home" SOUL_PLAYER_FOLDER="$music" SOUL_PLAYER_AUTOPLAY=1 \
  RUST_LOG="${RUST_LOG:-info,soul_player_rune=debug}" \
  "$root/app/rune/target/release/soul-player" >"$work/app.log" 2>&1 &
app_pid=$!

# ---- helpers ------------------------------------------------------------------------
np() { media-control get 2>/dev/null || echo null; }
field() { np | python3 -c "import json,sys; d=json.load(sys.stdin) or {}; v=d.get('$1'); print(len(v) if '$1'=='artworkData' and v else (v if v is not None else ''))"; }
fail() {
  echo "FAIL: $*"
  echo "--- now playing:"; np | python3 -c 'import json,sys; d=json.load(sys.stdin) or {}; d.pop("artworkData",None); print(d)'
  echo "--- app stdout/stderr:"; tail -20 "$work/app.log"
  echo "--- app log file:"; tail -40 "$home/Library/Logs/Soul Player/"*.log 2>/dev/null || echo "(none)"
  echo "--- now-playing cover cache:"; ls -la "$home/Library/Caches/Soul Player/now-playing" 2>/dev/null || echo "(none)"
  exit 1
}
# wait_for FIELD EXPECTED [TIMEOUT_S]
wait_for() {
  local f="$1" want="$2" t="${3:-15}" got=""
  for _ in $(seq 1 $((t * 4))); do
    got="$(field "$f")"
    [ "$got" = "$want" ] && return 0
    sleep 0.25
  done
  fail "$f: expected '$want', got '$got'"
}
ok() { echo "  ok  $*"; }

echo "== Now Playing e2e (exclusive=${EXCLUSIVE:-0})"
wait_for processIdentifier "$app_pid" 20;     ok "app is the system Now Playing app (pid $app_pid)"
wait_for title "Soul NP First";              ok "title"
wait_for artist "Soul E2E Artist";           ok "artist"
wait_for album "Now Playing Album";          ok "album"
wait_for duration "60";                      ok "duration"
wait_for playing "True";                     ok "playing"
# 30 s: macOS loads the cover asynchronously, and a cold CI VM has been seen to need >15 s.
for _ in $(seq 1 120); do [ "$(field artworkData)" != "" ] && [ "$(field artworkData)" -gt 0 ] && break; sleep 0.25; done
[ "$(field artworkData)" != "" ] && [ "$(field artworkData)" -gt 0 ] || fail "no artwork"
ok "artwork ($(field artworkData) bytes)"

media-control pause;           wait_for playing "False";  ok "remote pause"
media-control play;            wait_for playing "True";   ok "remote play"
media-control seek 30
for _ in $(seq 1 40); do e="$(field elapsedTime)"; python3 -c "import sys; sys.exit(0 if 29<=float('${e:-0}')<=35 else 1)" && break; sleep 0.25; done
python3 -c "import sys; sys.exit(0 if 29<=float('$(field elapsedTime)')<=35 else 1)" || fail "seek to 30s (elapsed $(field elapsedTime))"
ok "remote seek → $(field elapsedTime)s"
media-control next-track;      wait_for title "Soul NP Second"; ok "remote next"
media-control toggle-play-pause; wait_for playing "False"; ok "remote toggle (pause)"
media-control toggle-play-pause; wait_for playing "True";  ok "remote toggle (play)"

if [ "${EXCLUSIVE:-0}" = "1" ]; then
  grep -q 'got_exclusive=true' "$home/Library/Logs/Soul Player/"*.log \
    || fail "exclusive output did not engage"
  ok "hog-mode exclusive was active throughout"
fi
echo "PASS"
