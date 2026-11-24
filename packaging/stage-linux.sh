#!/usr/bin/env bash
# Unpack a built .deb into a stable staging tree that the RPM and the Flatpak
# both build from.
#
# Why go through the .deb rather than assembling each package from scratch:
# cargo-packager generates the desktop entry and the icon set from
# `[package.metadata.packager]` in app/rune/Cargo.toml. That metadata is the one
# place those values are written down, and it is already verified end to end
# (the AppImage's entry is checked in CI). Re-deriving them per format would
# give three chances to disagree about the app's name, category or icon — and a
# desktop entry that disagrees with the window's `app_id` costs the icon
# silently, which is exactly the class of bug this repo keeps finding.
#
# Usage: packaging/stage-linux.sh <path-to.deb> <staging-dir>
set -euo pipefail

# Resolved to an absolute path up front: the `ar` fallback below runs after a
# `cd` into the work dir, where a relative path no longer points anywhere. That
# only shows on a host without `dpkg-deb` (which reads the file from the
# original CWD), so it hides on Debian and Ubuntu and breaks everywhere else.
deb=$(readlink -f "${1:?usage: stage-linux.sh <deb> <out-dir>}")
out=${2:?usage: stage-linux.sh <deb> <out-dir>}
[ -f "$deb" ] || { echo "no such .deb: $deb" >&2; exit 1; }

bin_name=soul-player
app_id=co.soulaudio.soulplayer

rm -rf "$out"
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# `dpkg-deb` where it exists (Debian/Ubuntu), else unpack the ar archive by
# hand — a .deb is just `ar` wrapping `data.tar.*`, and the RPM leg runs on a
# Fedora container that has no dpkg.
if command -v dpkg-deb >/dev/null 2>&1; then
  dpkg-deb -x "$deb" "$work"
else
  ( cd "$work" && ar x "$deb" )
  tar -xf "$work"/data.tar.* -C "$work"
fi

src_desktop="$work/usr/share/applications/$bin_name.desktop"
[ -f "$src_desktop" ] || { echo "no desktop entry in $deb at $src_desktop" >&2; exit 1; }

install -Dm755 "$work/usr/bin/$bin_name" "$out/$bin_name"

# The desktop entry is reused verbatim EXCEPT for the name and `Icon=`, both of
# which Flatpak requires to be the app id: it installs the entry as
# `$FLATPAK_ID.desktop`, and the icon has to be findable under the same name.
# Outside Flatpak the .deb/.rpm keep the binary-named entry, which is what their
# `app_id` is (the app resolves this at runtime — see `desktop_id()`).
sed "s/^Icon=.*/Icon=$app_id/" "$src_desktop" > "$out/$app_id.desktop"
cp "$src_desktop" "$out/$bin_name.desktop"

# Largest icon the .deb carries, so neither package ships a blurry one.
icon=$(find "$work/usr/share/icons" -name "$bin_name.png" -print0 2>/dev/null \
        | xargs -0 -r ls -S 2>/dev/null | head -1)
[ -n "$icon" ] || { echo "no icon in $deb under usr/share/icons" >&2; exit 1; }
install -Dm644 "$icon" "$out/$app_id.png"
install -Dm644 "$icon" "$out/$bin_name.png"

cp "$(dirname "$0")/flatpak/$app_id.metainfo.xml" "$out/"

echo "staged into $out:"
ls -la "$out"
