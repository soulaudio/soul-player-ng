#!/bin/sh
# Soul Player installer for Linux and macOS.
#
#   curl -fsSL https://player.soulaudio.co/install.sh | sh
#
# Or, if you would rather read it first (and you should, for anything you pipe
# into a shell):
#
#   curl -fsSL https://player.soulaudio.co/install.sh -o install.sh
#   less install.sh && sh install.sh
#
# Options (after `sh -s --` when piping):
#
#   --version 0.4.7   install that release instead of the latest
#   --uninstall       remove Soul Player (settings and library index are kept)
#   --uninstall --purge
#                     remove Soul Player and everything it stored
#
# Running it again updates an existing install in place.
#
# What it does: resolves the latest release, picks the right package for this
# OS, architecture, and package manager, verifies it against the release's
# published SHA256SUMS, and installs it. It refuses to install anything whose
# checksum does not match, and it never installs a package this system cannot
# run (the glibc floor is checked up front).
#
# Windows is not covered here: download the .exe installer from the releases
# page.
#
# This file is the only copy. website/ copies it into its public/ at build time
# (the copy is gitignored), so what player.soulaudio.co serves is always this
# file and the two cannot drift.
set -eu

REPO="${SOUL_PLAYER_REPO:-soulaudio/soul-player-ng}"
# SOUL_PLAYER_RELEASES_URL points the script at a mirror laid out like a GitHub
# releases page (latest/download/..., download/<tag>/...). The release workflow
# uses it to run this exact script against a release before it is published.
BASE="${SOUL_PLAYER_RELEASES_URL:-https://github.com/${REPO}/releases}"
# The lowest glibc the release binaries are built against; see docs/INSTALL.md.
MIN_GLIBC_MAJOR=2
MIN_GLIBC_MINOR=35

say() { printf '%s\n' "$*"; }
err() { printf 'install: %s\n' "$*" >&2; exit 1; }

want_version=
action=install
purge=
while [ $# -gt 0 ]; do
	case "$1" in
		--version)
			[ $# -ge 2 ] || err "--version needs a version, e.g. --version 0.4.7"
			want_version=${2#v}
			shift 2
			;;
		--version=*) want_version=${1#--version=}; want_version=${want_version#v}; shift ;;
		--uninstall) action=uninstall; shift ;;
		--purge) purge=1; shift ;;
		-h | --help)
			sed -n '2,/^set -eu/p' "$0" 2>/dev/null | sed -e '$d' -e 's/^# \{0,1\}//' ||
				say "see https://player.soulaudio.co/install.sh"
			exit 0
			;;
		*) err "unknown option: $1 (try --help)" ;;
	esac
done
[ -z "$purge" ] || [ "$action" = uninstall ] || err "--purge only goes with --uninstall"
case "$want_version" in
	"" | [0-9]*.[0-9]*.[0-9]*) ;;
	*) err "not a version: $want_version (expected something like 0.4.7)" ;;
esac

need() {
	command -v "$1" >/dev/null 2>&1 || err "this script needs $1, which is not installed"
}

# curl and wget are both common enough that requiring one specifically is rude.
fetch() {
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL --retry 3 --retry-all-errors --retry-delay 2 "$1" -o "$2"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO "$2" --tries=4 "$1"
	else
		err "this script needs curl or wget"
	fi
}

fetch_stdout() {
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL --retry 3 --retry-all-errors --retry-delay 2 "$1"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO- --tries=4 "$1"
	else
		err "this script needs curl or wget"
	fi
}

# ---------------------------------------------------------------- platform ---

os=$(uname -s)
arch=$(uname -m)

case "$arch" in
	x86_64 | amd64) arch=x86_64 ;;
	aarch64 | arm64) arch=aarch64 ;;
	*) err "unsupported architecture: $arch (x86_64 and aarch64 are built)" ;;
esac

case "$os" in
	Linux | Darwin) ;;
	MINGW* | MSYS* | CYGWIN*)
		err "Windows is not supported by this script. Download the .exe installer from ${BASE}/latest"
		;;
	*) err "unsupported OS: $os" ;;
esac

# Only elevate when the target actually needs root.
sudo_if_needed() {
	if [ "$(id -u)" -eq 0 ]; then
		"$@"
	else
		need sudo
		sudo "$@"
	fi
}

MAC_APP="/Applications/Soul Player.app"
APPIMAGE_BIN="${HOME}/.local/bin/soul-player"
DESKTOP_FILE="${HOME}/.local/share/applications/co.soulaudio.soulplayer.desktop"

# ---------------------------------------------------------------- uninstall ---

uninstall() {
	found=
	# --purge: the app knows where it keeps its settings, library index, caches
	# and logs on each OS, so it does that part first (only ever its own paths),
	# while the binary is still there. The same folders are then removed here
	# too, for when the app cannot run (an AppImage on a system without the
	# desktop libraries it links) or is already gone.
	if [ -n "$purge" ]; then
		if [ "$os" = Darwin ]; then
			exe="${MAC_APP}/Contents/MacOS/soul-player"
		elif [ -x "$APPIMAGE_BIN" ]; then
			exe=$APPIMAGE_BIN
		else
			exe=$(command -v soul-player 2>/dev/null || true)
		fi
		say "Removing Soul Player's settings and data..."
		if [ -n "$exe" ] && [ -x "$exe" ]; then
			# Extract-and-run: an AppImage then needs no FUSE.
			APPIMAGE_EXTRACT_AND_RUN=1 "$exe" --uninstall >/dev/null 2>&1 ||
				say "(the app could not run to do it; removing its folders directly)"
		fi
		# Kept in step with data_paths() in app/src/uninstall.rs.
		if [ "$os" = Darwin ]; then
			lib="${HOME}/Library"
			id=co.soulaudio.soulplayer
			set -- "${lib}/Application Support/soul-player" "${lib}/Caches/soul-player" \
				"${lib}/Logs/Soul Player" "${lib}/Saved Application State/${id}.savedState" \
				"${lib}/Preferences/${id}.plist" "${lib}/Caches/${id}" "${lib}/HTTPStorages/${id}"
		else
			set -- "${XDG_CONFIG_HOME:-${HOME}/.config}/soul-player" \
				"${XDG_CACHE_HOME:-${HOME}/.cache}/soul-player" \
				"${XDG_DATA_HOME:-${HOME}/.local/share}/soul-player" \
				"${XDG_STATE_HOME:-${HOME}/.local/state}/soul-player"
		fi
		for d in "$@"; do
			if [ -e "$d" ]; then rm -rf "$d" && say "removed ${d}"; fi
		done
	fi
	if [ "$os" = Darwin ]; then
		if [ -d "$MAC_APP" ]; then
			found=1
			if [ -w /Applications ]; then rm -rf "$MAC_APP"; else sudo_if_needed rm -rf "$MAC_APP"; fi
			say "Removed ${MAC_APP}."
		fi
	else
		if command -v dpkg >/dev/null 2>&1 && dpkg -s soul-player >/dev/null 2>&1; then
			found=1
			say "Removing the package with apt..."
			sudo_if_needed apt-get remove -y soul-player
		elif command -v rpm >/dev/null 2>&1 && rpm -q soul-player >/dev/null 2>&1; then
			found=1
			if command -v dnf >/dev/null 2>&1; then
				say "Removing the package with dnf..."
				sudo_if_needed dnf remove -y soul-player
			else
				say "Removing the package with zypper..."
				sudo_if_needed zypper --non-interactive remove soul-player
			fi
		fi
		if [ -e "$APPIMAGE_BIN" ] || [ -e "$DESKTOP_FILE" ]; then
			found=1
			rm -f "$APPIMAGE_BIN" "$DESKTOP_FILE"
			say "Removed ${APPIMAGE_BIN}."
		fi
	fi
	if [ -z "$found" ] && [ -z "$purge" ]; then
		say "Soul Player is not installed (nothing installed by this script was found)."
	elif [ -z "$purge" ]; then
		say "Done. Settings and the library index were kept; remove them too with --uninstall --purge."
	else
		say "Done. Soul Player and everything it stored are gone."
	fi
}

if [ "$action" = uninstall ]; then
	uninstall
	exit 0
fi

# The packages declare glibc 2.35, so apt/dnf would refuse anyway, but an
# AppImage would install happily and then fail to start. Say so up front.
check_glibc() {
	[ "$os" = Linux ] || return 0
	command -v ldd >/dev/null 2>&1 || return 0
	v=$(ldd --version 2>/dev/null | head -n1 | tr ' ' '\n' | grep -E '^[0-9]+\.[0-9]+$' | tail -n1) || return 0
	[ -n "$v" ] || return 0
	major=${v%%.*}
	minor=${v##*.}
	if [ "$major" -lt "$MIN_GLIBC_MAJOR" ] ||
		{ [ "$major" -eq "$MIN_GLIBC_MAJOR" ] && [ "$minor" -lt "$MIN_GLIBC_MINOR" ]; }; then
		err "glibc ${v} is below the ${MIN_GLIBC_MAJOR}.${MIN_GLIBC_MINOR} floor this build needs"
	fi
}
check_glibc

# ----------------------------------------------------------------- version ---

# latest.json lives at a stable URL (the in-app updater reads the same file), so
# resolving the version needs no API token and no rate limit.
if [ -n "$want_version" ]; then
	version=$want_version
	tag="v${version}"
	say "Installing ${tag} (asked for)."
else
	say "Resolving the latest release..."
	manifest=$(fetch_stdout "${BASE}/latest/download/latest.json") ||
		err "could not reach ${BASE}/latest/download/latest.json"
	version=$(printf '%s' "$manifest" | tr ',' '\n' | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' | head -n1)
	[ -n "$version" ] || err "could not read a version out of latest.json"
	tag="v${version}"
	say "Latest release is ${tag}."
fi

# ----------------------------------------------------------------- package ---

# Pick the package this system can actually install, preferring one its package
# manager owns (so updates arrive through apt/dnf) over a loose AppImage.
kind=
asset=
if [ "$os" = Darwin ]; then
	kind=dmg
	asset="Soul-Player_${version}_universal.dmg"
elif command -v dpkg >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
	kind=deb
	case "$arch" in
		x86_64) asset="soul-player_${version}_amd64.deb" ;;
		aarch64) asset="soul-player_${version}_arm64.deb" ;;
	esac
elif command -v rpm >/dev/null 2>&1 && { command -v dnf >/dev/null 2>&1 || command -v zypper >/dev/null 2>&1; }; then
	# openSUSE ships rpm with zypper and no dnf. Without this it fell through
	# to the AppImage, quietly contradicting the docs that promise an rpm.
	kind=rpm
	asset="soul-player-${version}-1.${arch}.rpm"
else
	kind=appimage
	asset="soul-player_${version}_${arch}.AppImage"
fi

url="${BASE}/download/${tag}/${asset}"

# --------------------------------------------------------------- download ---

tmp=$(mktemp -d)
cleanup() { rm -rf "$tmp"; }
trap cleanup EXIT INT TERM

say "Downloading ${asset}..."
fetch "$url" "${tmp}/${asset}" || err "download failed: ${url} (is ${tag} a published release?)"

say "Verifying checksum..."
fetch "${BASE}/download/${tag}/SHA256SUMS" "${tmp}/SHA256SUMS" ||
	err "could not fetch SHA256SUMS, refusing to install unverified"

# Parsed in the shell rather than with awk: a minimal openSUSE image has no
# awk, and the failure looked like "not listed in SHA256SUMS", which reads as
# a tampered download rather than a missing tool.
expected=
while read -r sum name; do
	name=${name#\*} # some sha256sum output marks binary mode with a leading *
	if [ "$name" = "$asset" ]; then
		expected=$sum
		break
	fi
done <"${tmp}/SHA256SUMS"
[ -n "$expected" ] || err "${asset} is not listed in SHA256SUMS"

if command -v sha256sum >/dev/null 2>&1; then
	actual=$(sha256sum "${tmp}/${asset}" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
	actual=$(shasum -a 256 "${tmp}/${asset}" | cut -d' ' -f1)
else
	err "this script needs sha256sum or shasum to verify the download"
fi

[ "$actual" = "$expected" ] ||
	err "checksum mismatch for ${asset}: expected ${expected}, got ${actual}. NOT installing."
say "Checksum OK."

# ---------------------------------------------------------------- install ---

case "$kind" in
	deb)
		say "Installing with apt..."
		sudo_if_needed apt-get install -y --allow-downgrades "${tmp}/${asset}"
		say "Done. Updates will arrive through apt."
		;;
	rpm)
		if command -v dnf >/dev/null 2>&1; then
			say "Installing with dnf..."
			sudo_if_needed dnf install -y "${tmp}/${asset}"
			# `install` of an older rpm than the one installed does nothing;
			# going back to a pinned version is an explicit downgrade.
			if [ "$(rpm -q --qf '%{VERSION}' soul-player 2>/dev/null)" != "$version" ]; then
				sudo_if_needed dnf downgrade -y "${tmp}/${asset}"
			fi
			say "Done. Updates will arrive through dnf."
		else
			say "Installing with zypper..."
			sudo_if_needed zypper --non-interactive install --allow-unsigned-rpm --oldpackage "${tmp}/${asset}"
			say "Done. Updates will arrive through zypper."
		fi
		;;
	*) ;;
esac

# What the package manager says is installed must be what was asked for: a
# package manager that quietly keeps another version is not an install.
installed=
case "$kind" in
	deb) installed=$(dpkg-query -W -f='${Version}' soul-player 2>/dev/null || true) ;;
	rpm) installed=$(rpm -q --qf '%{VERSION}' soul-player 2>/dev/null || true) ;;
esac
case "$kind" in
	deb | rpm) [ "$installed" = "$version" ] || err "the package manager reports soul-player '${installed}', expected ${version}" ;;
esac

case "$kind" in
	appimage)
		bindir="${HOME}/.local/bin"
		appdir="${HOME}/.local/share/applications"
		mkdir -p "$bindir" "$appdir"
		# Into place with a rename, so a running copy is never half-overwritten
		# and an update that fails leaves the old one working.
		install -m 0755 "${tmp}/${asset}" "${bindir}/.soul-player.new"
		mv -f "${bindir}/.soul-player.new" "${bindir}/soul-player"
		cat >"${appdir}/co.soulaudio.soulplayer.desktop" <<-DESKTOP
			[Desktop Entry]
			Type=Application
			Name=Soul Player
			Comment=Play your music library, gapless and bit-perfect
			Exec=${bindir}/soul-player %U
			Icon=co.soulaudio.soulplayer
			Categories=AudioVideo;Audio;Player;
			Terminal=false
		DESKTOP
		say "Installed to ${bindir}/soul-player."
		case ":${PATH}:" in
			*":${bindir}:"*) ;;
			*) say "Note: ${bindir} is not on your PATH." ;;
		esac
		say "The AppImage updates itself from inside the app."
		;;
	dmg)
		say "Mounting the disk image..."
		mountpoint=$(hdiutil attach -nobrowse -readonly "${tmp}/${asset}" |
			grep -o '/Volumes/.*' | head -n1)
		[ -n "$mountpoint" ] || err "could not mount ${asset}"
		# Detach even if the copy fails, so a failed run does not leave the
		# image mounted.
		trap 'hdiutil detach "$mountpoint" >/dev/null 2>&1 || true; cleanup' EXIT INT TERM
		[ -d "${mountpoint}/Soul Player.app" ] || err "${asset} does not contain Soul Player.app"
		# /Applications is writable for admin accounts; a standard account
		# needs sudo for it.
		as=
		[ -w /Applications ] || { need sudo; as=sudo; say "/Applications needs administrator rights; sudo may ask for your password."; }
		staged="/Applications/.Soul Player.app.installing"
		$as rm -rf "$staged"
		say "Copying Soul Player.app to /Applications..."
		# ditto keeps the bundle's symlinks, permissions and extended attributes
		# exactly as built; cp -R does not always.
		$as ditto "${mountpoint}/Soul Player.app" "$staged" || { $as rm -rf "$staged"; err "copy failed; the existing app was left alone"; }
		got=$(defaults read "${staged}/Contents/Info" CFBundleShortVersionString 2>/dev/null || true)
		[ "$got" = "$version" ] || { $as rm -rf "$staged"; err "the copied app reports version '${got}', expected ${version}; NOT installed"; }
		# The old app is only removed once the new one is complete next to it.
		if pgrep -x soul-player >/dev/null 2>&1; then
			say "Soul Player is running; quit and reopen it to use ${version}."
		fi
		$as rm -rf "$MAC_APP"
		$as mv "$staged" "$MAC_APP"
		say "Done. Open Soul Player from Applications. Later updates install from inside the app."
		;;
esac
