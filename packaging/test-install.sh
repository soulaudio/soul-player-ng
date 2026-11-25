#!/bin/sh
# Test the PUBLISHED installer the way a user runs it:
#
#   curl -fsSL https://player.soulaudio.co/install.sh | sh
#
# Not the copy in this repo, and not a release artifact: the script actually
# being served, resolving the release it actually resolves. That makes this a
# test of the whole chain (site deployed, latest.json current, assets
# downloadable, checksums matching, package manager accepting it), which is
# the chain that broke in 0.4.6 when one rpm asset returned 500 from GitHub
# while every other asset was fine.
#
# Usage, inside the target system:
#   test-install.sh <mode> [expected-version]
#
#   deb       expect a dpkg package at the expected version
#   rpm       expect an rpm package at the expected version
#   appimage  expect an executable in ~/.local/bin plus a desktop entry
#   macos     expect Soul Player.app in /Applications at the expected version
#   refuse    expect a NON-zero exit and nothing installed (unsupported system)
#
# With no expected version, the version check is skipped and only presence is
# asserted.
set -eu

MODE=${1:?usage: test-install.sh <deb|rpm|appimage|macos|refuse> [version]}
WANT=${2:-}
URL=${SOUL_PLAYER_INSTALL_URL:-https://player.soulaudio.co/install.sh}

fail() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}
ok() { printf 'ok: %s\n' "$*"; }

check_version() {
	[ -n "$WANT" ] || return 0
	[ "$1" = "$WANT" ] || fail "installed $1, expected $WANT"
	ok "version is $1"
}

echo "running the published installer from $URL"
set +e
curl -fsSL "$URL" | sh
status=$?
set -e

if [ "$MODE" = refuse ]; then
	[ "$status" -ne 0 ] || fail "the installer succeeded on a system it should refuse"
	ok "refused with exit $status"
	exit 0
fi

[ "$status" -eq 0 ] || fail "the installer exited $status"

case "$MODE" in
	deb)
		dpkg -s soul-player >/dev/null 2>&1 || fail "no soul-player package"
		check_version "$(dpkg-query -W -f='${Version}' soul-player)"
		command -v soul-player >/dev/null || fail "soul-player is not on PATH"
		ok "deb installed, binary at $(command -v soul-player)"
		;;
	rpm)
		rpm -q soul-player >/dev/null 2>&1 || fail "no soul-player package"
		check_version "$(rpm -q --qf '%{VERSION}' soul-player)"
		command -v soul-player >/dev/null || fail "soul-player is not on PATH"
		ok "rpm installed, binary at $(command -v soul-player)"
		;;
	appimage)
		bin="$HOME/.local/bin/soul-player"
		[ -x "$bin" ] || fail "no executable at $bin"
		# An AppImage is an ELF; catching an HTML error page saved as the
		# binary is the point.
		head -c 4 "$bin" | od -An -tx1 | grep -q '7f 45 4c 46' || fail "$bin is not an ELF"
		[ "$(wc -c <"$bin")" -gt 10000000 ] || fail "$bin is implausibly small"
		[ -f "$HOME/.local/share/applications/co.soulaudio.soulplayer.desktop" ] ||
			fail "no desktop entry"
		ok "AppImage installed at $bin"
		;;
	macos)
		app="/Applications/Soul Player.app"
		[ -d "$app" ] || fail "no app at $app"
		check_version "$(defaults read "$app/Contents/Info" CFBundleShortVersionString)"
		ok "app installed at $app"
		;;
	*) fail "unknown mode: $MODE" ;;
esac
