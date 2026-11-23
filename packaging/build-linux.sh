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
bin="$app/target/release/soul-player-rune"
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
