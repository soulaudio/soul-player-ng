#!/usr/bin/env bash
# Build every Linux package from one already-built release binary: .deb, .rpm
# and the staged tree the Flatpak is assembled from.
#
# One script rather than three CI steps because the formats are not independent
# — the RPM and the Flatpak both take their desktop entry and icon out of the
# .deb (see stage-linux.sh for why), so the order matters and getting it wrong
# produces packages that disagree about the app's identity.
#
# Usage: packaging/build-linux.sh [--skip-build]
#   Run from the repo root. Expects cargo-packager and cargo-generate-rpm.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/app/rune"
bin="$app/target/release/soul-player"
version=$(grep -m1 '^version = ' "$app/Cargo.toml" | cut -d'"' -f2)

if [ "${1:-}" != "--skip-build" ]; then
  echo "==> building the release binary"
  ( cd "$app" && cargo build --release --locked )
fi
[ -f "$bin" ] || { echo "no release binary at $bin" >&2; exit 1; }

# Strip before packaging, not via `[profile.release]`, so this stays a LINUX
# packaging decision: macOS needs its symbols for the signed bundle's crash
# reports, and Windows keeps its own. A full strip (symbols as well as debug
# info) takes ~23% off the binary and ~30% off the .deb — worth it for
# something every user downloads, and backtraces are off by default anyway
# (`panic.backtrace=disabled`). It also clears lintian's
# `unstripped-binary-or-object`, which `--strip-debug` alone does not: that
# leaves `.symtab` behind and saves only 2%.
echo "==> stripping"
before=$(stat -c%s "$bin")
strip "$bin"
echo "    $(numfmt --to=iec "$before") -> $(numfmt --to=iec "$(stat -c%s "$bin")")"

# Debian wants a changelog at /usr/share/doc/<pkg>/changelog.gz; without one
# lintian errors. Generated rather than committed so it cannot go stale against
# the version actually being built, and `-n` keeps gzip from embedding a
# timestamp so the package stays reproducible.
# The .deb declares `libc6 (>= X)`. That number is a promise about which
# releases the binary loads on, and it is only true by inspection — so check it
# against the binary rather than trusting whoever edited Cargo.toml last.
#
# The floor is the highest NON-WEAK glibc version referenced: weak versioned
# symbols (Rust's std uses pidfd_spawnp/pidfd_getpid from 2.39) resolve to an
# older fallback when absent, so counting them would overstate the requirement
# and turn away users whose machines are fine.
echo "==> glibc floor"
need=$(objdump -T "$bin" | awk '$2!="w" && /GLIBC_/ {
         for (i=1;i<=NF;i++) if ($i ~ /^\(GLIBC_/) { gsub(/[()]|GLIBC_/,"",$i); print $i }
       }' | sort -uV | tail -1)
declared=$(grep -oE '"libc6 \(>= [0-9.]+\)"' "$app/Cargo.toml" | grep -oE '[0-9]+\.[0-9]+' | head -1)
echo "    binary needs glibc $need; .deb declares $declared"
[ "$need" = "$declared" ] || {
  echo "mismatch: the .deb promises libc6 >= $declared but the binary needs $need." >&2
  echo "Update [package.metadata.packager.deb] depends in app/rune/Cargo.toml." >&2
  exit 1
}

echo "==> changelog"
mkdir -p "$root/packaging/deb/generated"
{
  echo "soul-player ($version) stable; urgency=medium"
  echo
  echo "  * Soul Player $version."
  echo "    Release notes: https://github.com/soulaudio/soul-player-ng/releases/tag/v$version"
  echo
  echo " -- datadir s. r. o. <hello@datadir.co>  $(date -R)"
} | gzip -9n > "$root/packaging/deb/generated/changelog.gz"

echo "==> .deb"
( cd "$app" && cargo packager --release -f deb )
deb=$(ls "$app"/target/release/*.deb | head -1)
echo "    $deb"

# Declare the transition off the old package name.
#
# Through 0.4.1 this package was `soul-player-rune` (its name came from the
# binary's). To dpkg, `soul-player` is an unrelated package, so without these
# three fields a 0.4.1 user ends up with BOTH installed: two binaries under
# different names and two desktop entries, with no error anywhere. Verified by
# building both packages — with the fields dpkg reports "Removing
# soul-player-rune ... to allow configuration of soul-player"; without them
# `dpkg -l` lists both.
#
# They are patched in here rather than declared in Cargo.toml because
# cargo-packager 0.11.8's Debian backend has no field for them (its
# provides/conflicts/replaces belong to the pacman config; writing them under
# `[package.metadata.packager.deb]` fails the build with "unknown field
# `conflicts`"). The three go together: Conflicts+Replaces are what let dpkg
# remove the old package instead of refusing the file overlap, and Provides
# keeps anything that depended on the old name satisfied (Debian policy §7.6).
echo "==> .deb control: transition from soul-player-rune"
command -v dpkg-deb >/dev/null 2>&1 || {
  echo "dpkg-deb is required to declare the transition off the old package" >&2
  echo "name. Without it the .deb would install ALONGSIDE a 0.4.1 install" >&2
  echo "instead of replacing it, so this is a hard failure rather than a" >&2
  echo "warning. Install dpkg (it is in the 'rune' nix dev shell)." >&2
  exit 1
}
debwork=$(mktemp -d)
trap 'rm -rf "$debwork"' EXIT
dpkg-deb -R "$deb" "$debwork"
for field in Provides Conflicts Replaces; do
  # Appended to the control file's single paragraph. `Description` is a
  # multi-line field that runs to the end, so new fields have to go BEFORE it
  # or they would be swallowed as part of its continuation lines.
  grep -q "^$field:" "$debwork/DEBIAN/control" && continue
  sed -i "0,/^Description:/s//$field: soul-player-rune\nDescription:/" \
    "$debwork/DEBIAN/control"
done
dpkg-deb -b "$debwork" "$deb" >/dev/null
# Prove it landed: a silently-unpatched control file is the whole failure mode.
for field in Provides Conflicts Replaces; do
  dpkg-deb -f "$deb" "$field" | grep -q "soul-player-rune" || {
    echo "$field was not written to the .deb's control file" >&2
    dpkg-deb -f "$deb" >&2
    exit 1
  }
done
echo "    Provides/Conflicts/Replaces: soul-player-rune"
rm -rf "$debwork"
trap - EXIT

echo "==> staging shared files (desktop entry + icon, from the .deb)"
stage="$app/target/release/linux-stage"
"$root/packaging/stage-linux.sh" "$deb" "$stage" >/dev/null
echo "    $stage"

echo "==> .rpm"
( cd "$app" && cargo generate-rpm )
rpm=$(ls "$app"/target/generate-rpm/*.rpm 2>/dev/null | head -1 || true)
echo "    ${rpm:-<none>}"

echo
echo "built:"
ls -la "$deb" ${rpm:+"$rpm"} 2>/dev/null
