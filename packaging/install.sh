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
BASE="https://github.com/${REPO}/releases"
# The lowest glibc the release binaries are built against; see docs/INSTALL.md.
MIN_GLIBC_MAJOR=2
MIN_GLIBC_MINOR=35

say() { printf '%s\n' "$*"; }
err() { printf 'install: %s\n' "$*" >&2; exit 1; }

need() {
	command -v "$1" >/dev/null 2>&1 || err "this script needs $1, which is not installed"
}

# curl and wget are both common enough that requiring one specifically is rude.
fetch() {
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL "$1" -o "$2"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO "$2" "$1"
	else
		err "this script needs curl or wget"
	fi
}

fetch_stdout() {
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL "$1"
	elif command -v wget >/dev/null 2>&1; then
		wget -qO- "$1"
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
say "Resolving the latest release..."
manifest=$(fetch_stdout "${BASE}/latest/download/latest.json") ||
	err "could not reach ${BASE}/latest/download/latest.json"
version=$(printf '%s' "$manifest" | tr ',' '\n' | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' | head -n1)
[ -n "$version" ] || err "could not read a version out of latest.json"
tag="v${version}"
say "Latest release is ${tag}."

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
fetch "$url" "${tmp}/${asset}" || err "download failed: ${url}"

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

# Only elevate when the install target actually needs root.
sudo_if_needed() {
	if [ "$(id -u)" -eq 0 ]; then
		"$@"
	else
		need sudo
		sudo "$@"
	fi
}

case "$kind" in
	deb)
		say "Installing with apt..."
		sudo_if_needed apt-get install -y "${tmp}/${asset}"
		say "Done. Updates will arrive through apt."
		;;
	rpm)
		if command -v dnf >/dev/null 2>&1; then
			say "Installing with dnf..."
			sudo_if_needed dnf install -y "${tmp}/${asset}"
			say "Done. Updates will arrive through dnf."
		else
			say "Installing with zypper..."
			sudo_if_needed zypper --non-interactive install --allow-unsigned-rpm "${tmp}/${asset}"
			say "Done. Updates will arrive through zypper."
		fi
		;;
	appimage)
		bindir="${HOME}/.local/bin"
		appdir="${HOME}/.local/share/applications"
		mkdir -p "$bindir" "$appdir"
		install -m 0755 "${tmp}/${asset}" "${bindir}/soul-player"
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
		say "Copying Soul Player.app to /Applications..."
		rm -rf "/Applications/Soul Player.app"
		cp -R "${mountpoint}/Soul Player.app" /Applications/
		say "Done. Open it from Applications."
		;;
esac
