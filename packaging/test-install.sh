#!/bin/sh
# Test the installer the way a user runs it, through its whole life:
#
#   curl -fsSL https://player.soulaudio.co/install.sh | sh
#
# By default the script actually being served, resolving the release it
# actually resolves: a test of the whole chain (site deployed, latest.json
# current, assets downloadable, checksums matching, package manager accepting
# it). 0.4.6 shipped with one rpm asset returning 500 from GitHub while every
# other asset was fine; this is what notices.
#
# Before a release is published, the release workflow points the same test at
# a local mirror of the candidate (SOUL_PLAYER_INSTALL_URL for the script,
# SOUL_PLAYER_RELEASES_URL for the releases), so a release whose installer path
# is broken never goes out.
#
# Usage, inside the target system:
#   test-install.sh <mode> [version] [previous]
#
#   deb       a dpkg package
#   rpm       an rpm package (dnf or zypper)
#   appimage  an executable in ~/.local/bin plus a desktop entry
#   macos     Soul Player.app in /Applications
#   refuse    a NON-zero exit and nothing installed (unsupported system)
#
# The life it walks (each step checked, not just its exit status):
#   1. install `previous` (when given), then update to the latest: the update
#      path, with settings surviving it
#   2. run the installer again: nothing changes
#   3. --uninstall: the app goes, settings stay
#   4. install again, then --uninstall --purge: the app and its data go
#
# With no version the version checks are skipped and only presence is asserted.
set -eu

MODE=${1:?usage: test-install.sh <deb|rpm|appimage|macos|refuse> [version] [previous]}
WANT=${2:-}
PREV=${3:-}
URL=${SOUL_PLAYER_INSTALL_URL:-https://player.soulaudio.co/install.sh}

fail() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}
ok() { printf 'ok: %s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

installer() {
	echo "\$ curl -fsSL $URL | sh -s -- $*"
	curl -fsSL --retry 3 --retry-all-errors "$URL" | sh -s -- "$@"
}

MAC_APP="/Applications/Soul Player.app"
APPIMAGE_BIN="$HOME/.local/bin/soul-player"
DESKTOP_FILE="$HOME/.local/share/applications/co.soulaudio.soulplayer.desktop"

if [ "$MODE" = macos ]; then
	DATA="$HOME/Library/Application Support/soul-player"
	CACHE="$HOME/Library/Caches/soul-player"
else
	DATA="${XDG_CONFIG_HOME:-$HOME/.config}/soul-player"
	CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/soul-player"
fi

# Stand-ins for a user's settings and library index, so "kept" and "purged"
# are checked against something real.
seed_data() {
	mkdir -p "$DATA" "$CACHE"
	printf 'volume = 0.42\n' >"$DATA/config.toml"
	printf 'x' >"$CACHE/marker"
}
data_kept() {
	[ -f "$DATA/config.toml" ] || fail "$1 deleted the settings ($DATA)"
	grep -q 'volume = 0.42' "$DATA/config.toml" || fail "$1 rewrote the settings"
	ok "settings survived $1"
}
data_gone() {
	for d in "$DATA" "$CACHE"; do
		[ ! -e "$d" ] || fail "--purge left $d"
	done
	ok "--purge removed the settings and caches"
}

exe() {
	case "$MODE" in
		macos) echo "$MAC_APP/Contents/MacOS/soul-player" ;;
		appimage) echo "$APPIMAGE_BIN" ;;
		*) hash -r 2>/dev/null || true; command -v soul-player || true ;;
	esac
}

installed_version() {
	case "$MODE" in
		deb) dpkg-query -W -f='${Version}' soul-player 2>/dev/null || true ;;
		rpm) rpm -q --qf '%{VERSION}' soul-player 2>/dev/null || true ;;
		macos) defaults read "$MAC_APP/Contents/Info" CFBundleShortVersionString 2>/dev/null || true ;;
		appimage) [ -x "$APPIMAGE_BIN" ] && echo present || true ;;
	esac
}

# The installed binary answers --version without a display. Proves the thing
# that is installed actually runs, which package metadata alone does not.
# Releases before 0.4.7 opened a window for --version, so only the version
# under test is asked.
check_runs() {
	if [ -n "${SOUL_PLAYER_SKIP_RUN_CHECK:-}" ]; then
		ok "skipping the run check (asked for)"
		return 0
	fi
	bin=$(exe)
	[ -n "$bin" ] && [ -x "$bin" ] || fail "no executable to run ($bin)"
	reported=$(APPIMAGE_EXTRACT_AND_RUN=1 "$bin" --version 2>/dev/null) || fail "$bin --version exited non-zero"
	case "$reported" in
		"soul-player "*) ok "it runs: $reported" ;;
		*) fail "--version printed: $reported" ;;
	esac
	[ -z "$WANT" ] || [ "$reported" = "soul-player $WANT" ] ||
		fail "--version said '$reported', expected 'soul-player $WANT'"
}

check_installed() { # [expected version]
	want=${1:-}
	case "$MODE" in
		deb) dpkg -s soul-player >/dev/null 2>&1 || fail "no soul-player package" ;;
		rpm) rpm -q soul-player >/dev/null 2>&1 || fail "no soul-player package" ;;
		macos)
			[ -d "$MAC_APP" ] || fail "no app at $MAC_APP"
			[ ! -e "/Applications/.Soul Player.app.installing" ] || fail "the staging copy was left behind"
			# The installer must not leave its disk image mounted.
			! hdiutil info | grep -q '/Volumes/Soul Player' || fail "the disk image is still mounted"
			;;
		appimage)
			bin=$APPIMAGE_BIN
			[ -x "$bin" ] || fail "no executable at $bin"
			# An AppImage is an ELF; catching an HTML error page saved as the
			# binary is the point.
			head -c 4 "$bin" | od -An -tx1 | grep -q '7f 45 4c 46' || fail "$bin is not an ELF"
			[ "$(wc -c <"$bin")" -gt 10000000 ] || fail "$bin is implausibly small"
			# e_machine sits at offset 18 of the ELF header, little endian:
			# 3e00 is x86-64, b700 is aarch64. A wrong-architecture AppImage
			# installs perfectly happily and then fails to exec.
			machine=$(od -An -tx1 -j18 -N2 "$bin" | tr -d ' \n')
			case "$(uname -m)" in
				x86_64) arch_want=3e00 ;;
				aarch64 | arm64) arch_want=b700 ;;
				*) arch_want= ;;
			esac
			[ -z "$arch_want" ] || [ "$machine" = "$arch_want" ] ||
				fail "$bin is built for e_machine $machine, this is $(uname -m)"
			[ ! -e "$HOME/.local/bin/.soul-player.new" ] || fail "the staging copy was left behind"
			[ -f "$DESKTOP_FILE" ] || fail "no desktop entry"
			;;
	esac
	if [ -n "$want" ] && [ "$MODE" != appimage ]; then
		got=$(installed_version)
		[ "$got" = "$want" ] || fail "installed $got, expected $want"
		ok "$MODE at $got"
	else
		ok "$MODE installed"
	fi
}

check_removed() {
	case "$MODE" in
		deb) ! dpkg -s soul-player >/dev/null 2>&1 || fail "the package is still installed" ;;
		rpm) ! rpm -q soul-player >/dev/null 2>&1 || fail "the package is still installed" ;;
		macos) [ ! -e "$MAC_APP" ] || fail "$MAC_APP is still there" ;;
		appimage)
			[ ! -e "$APPIMAGE_BIN" ] || fail "$APPIMAGE_BIN is still there"
			[ ! -e "$DESKTOP_FILE" ] || fail "the desktop entry is still there"
			;;
	esac
	case "$MODE" in
		deb | rpm) hash -r 2>/dev/null || true; ! command -v soul-player >/dev/null 2>&1 || fail "soul-player is still on PATH" ;;
	esac
	ok "the app is gone"
}

# macOS only: open the GUI for a few seconds. A runner has a window server,
# and a bundle that is damaged in a way --version cannot see (resources,
# frameworks, Info.plist) dies here.
check_launches() {
	[ "$MODE" = macos ] && [ -z "${SOUL_PLAYER_SKIP_RUN_CHECK:-}" ] || return 0
	SOUL_PLAYER_NO_UPDATE=1 "$(exe)" >/dev/null 2>&1 &
	pid=$!
	sleep 8
	kill -0 "$pid" 2>/dev/null || fail "the app exited at launch"
	kill -TERM "$pid"
	wait "$pid" 2>/dev/null || true
	ok "the app launches"
}

# ------------------------------------------------------------------- refuse ---

if [ "$MODE" = refuse ]; then
	set +e
	installer
	status=$?
	set -e
	[ "$status" -ne 0 ] || fail "the installer succeeded on a system it should refuse"
	ok "refused with exit $status"
	exit 0
fi

# ---------------------------------------------------------------- lifecycle ---

if [ -n "$PREV" ] && [ "$PREV" != "$WANT" ]; then
	step "1a. install the previous release, $PREV"
	installer --version "$PREV" || fail "installing $PREV exited $?"
	check_installed "$PREV"
	seed_data
	step "1b. update to the latest"
	installer || fail "the update exited $?"
	check_installed "$WANT"
	check_runs
	data_kept "the update"
else
	step "1. install the latest"
	installer || fail "the installer exited $?"
	check_installed "$WANT"
	check_runs
	seed_data
fi
check_launches

step "2. run the installer again"
installer || fail "a second run exited $?"
check_installed "$WANT"
check_runs
data_kept "a second run"

step "3. --uninstall"
installer --uninstall || fail "--uninstall exited $?"
check_removed
data_kept "--uninstall"

step "4. install again, then --uninstall --purge"
installer || fail "reinstalling exited $?"
check_installed "$WANT"
installer --uninstall --purge || fail "--uninstall --purge exited $?"
check_removed
data_gone

printf '\nPASS: %s %s%s: install, update, reinstall, uninstall, purge\n' "$MODE" "${WANT:-latest}" "${PREV:+ (from $PREV)}"
