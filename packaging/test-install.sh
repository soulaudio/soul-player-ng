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

# The installed binary answers --version without a display. Proves the thing
# that is installed actually runs, which package metadata alone does not.
#
# Needs a release that HAS the flag: before it was fixed, --version fell
# through to the event loop and panicked on any headless machine. Testing
# 0.4.6 or older will fail here, correctly.
check_runs() {
	if [ -n "${SOUL_PLAYER_SKIP_RUN_CHECK:-}" ]; then
		ok "skipping the run check (asked for)"
		return 0
	fi
	reported=$(soul-player --version 2>/dev/null) || fail "--version exited non-zero"
	case "$reported" in
		"soul-player "*) ok "it runs: $reported" ;;
		*) fail "--version printed: $reported" ;;
	esac
	[ -z "$WANT" ] || [ "$reported" = "soul-player $WANT" ] ||
		fail "--version said '$reported', expected 'soul-player $WANT'"
}

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
		check_runs
		ok "deb installed, binary at $(command -v soul-player)"
		;;
	rpm)
		rpm -q soul-player >/dev/null 2>&1 || fail "no soul-player package"
		check_version "$(rpm -q --qf '%{VERSION}' soul-player)"
		command -v soul-player >/dev/null || fail "soul-player is not on PATH"
		check_runs
		ok "rpm installed, binary at $(command -v soul-player)"
		;;
	appimage)
		bin="$HOME/.local/bin/soul-player"
		[ -x "$bin" ] || fail "no executable at $bin"
		# An AppImage is an ELF; catching an HTML error page saved as the
		# binary is the point.
		head -c 4 "$bin" | od -An -tx1 | grep -q '7f 45 4c 46' || fail "$bin is not an ELF"
		[ "$(wc -c <"$bin")" -gt 10000000 ] || fail "$bin is implausibly small"
		# e_machine sits at offset 18 of the ELF header, little endian:
		# 3e00 is x86-64, b700 is aarch64. A wrong-architecture AppImage
		# installs perfectly happily and then fails to exec, so the ELF check
		# alone is not enough.
		machine=$(od -An -tx1 -j18 -N2 "$bin" | tr -d ' 
')
		case "$(uname -m)" in
			x86_64) want=3e00 ;;
			aarch64 | arm64) want=b700 ;;
			*) want= ;;
		esac
		if [ -n "$want" ] && [ "$machine" != "$want" ]; then
			fail "$bin is built for e_machine $machine, this is $(uname -m) (expected $want)"
		fi
		ok "built for $(uname -m)"
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
