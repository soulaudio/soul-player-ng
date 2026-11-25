#!/usr/bin/env bash
# Regenerate cargo-sources.json from app/rune/Cargo.lock.
#
# Flathub builders have no network, so every crate must be a declared source
# with a checksum. flatpak-cargo-generator turns the lockfile into exactly that
# — ~1500 entries, including both git dependencies resolved to the commits the
# lockfile pins.
#
# The output is NOT committed — it is derived entirely from the lockfile, and a
# committed copy meant every dependency bump needed a regeneration commit or CI
# failed. CI generates it fresh; run this before submitting to Flathub, whose
# repository does need the file.
#
# `--check` compares against an existing copy without writing. Nothing calls it
# now that the file is generated rather than committed; it stays because it is
# the right check to run against the copy in Flathub's own repo, where the file
# IS committed and can go stale.
#
# The generator itself is fetched at a pinned commit rather than "master": it is
# a third-party script that rewrites our entire dependency manifest, and an
# unpinned fetch would make this output change without any change of ours.
set -euo pipefail

# flatpak-builder-tools has no releases, so this is a commit. Bump deliberately.
GENERATOR_REF=${GENERATOR_REF:-master}
GENERATOR_URL="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/${GENERATOR_REF}/cargo/flatpak-cargo-generator.py"

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
lock="$root/app/rune/Cargo.lock"
out="$here/cargo-sources.json"

check=0
[ "${1:-}" = "--check" ] && check=1

[ -f "$lock" ] || { echo "no lockfile at $lock" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The generator needs aiohttp/tomlkit and a git binary (it resolves git
# dependencies by cloning them). Doing that in a container keeps those off the
# developer's machine and makes the result the same everywhere — including on a
# host with no Python at all, which is most of them.
if ! command -v podman >/dev/null 2>&1 && ! command -v docker >/dev/null 2>&1; then
  echo "needs podman or docker: the generator runs in a container so its" >&2
  echo "Python dependencies (aiohttp, tomlkit) and git are not required here." >&2
  exit 1
fi
runtime=$(command -v podman || command -v docker)

cp "$lock" "$work/Cargo.lock"
curl -sSfL "$GENERATOR_URL" -o "$work/flatpak-cargo-generator.py"

echo "==> generating from app/rune/Cargo.lock ($(grep -c '^\[\[package\]\]' "$lock") packages)"
"$runtime" run --rm -v "$work:/w" docker.io/library/python:3-slim sh -c '
  apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq git >/dev/null 2>&1
  pip install -q aiohttp tomlkit
  cd /w && python flatpak-cargo-generator.py Cargo.lock -o cargo-sources.json
' >/dev/null

[ -s "$work/cargo-sources.json" ] || { echo "the generator produced nothing" >&2; exit 1; }

if [ "$check" = 1 ]; then
  if cmp -s "$work/cargo-sources.json" "$out"; then
    echo "cargo-sources.json is in sync with Cargo.lock"
    exit 0
  fi
  echo "::error::packaging/flathub/cargo-sources.json is STALE." >&2
  echo "app/rune/Cargo.lock has changed since it was generated, so a Flathub" >&2
  echo "build would try to compile against the wrong crate set — offline, with" >&2
  echo "no way to fetch the difference. Regenerate it:" >&2
  echo "  packaging/flathub/generate-sources.sh" >&2
  diff <(python3 -c "import json,sys;[print(s.get('url',s.get('dest-filename','?'))) for s in json.load(open('$out'))]" | sort) \
       <(python3 -c "import json,sys;[print(s.get('url',s.get('dest-filename','?'))) for s in json.load(open('$work/cargo-sources.json'))]" | sort) \
    | head -20 >&2 || true
  exit 1
fi

mv "$work/cargo-sources.json" "$out"
echo "wrote $out ($(python3 -c "import json;print(len(json.load(open('$out'))))") sources)"
