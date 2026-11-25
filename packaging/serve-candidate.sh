#!/usr/bin/env bash
# Serve a release that is not published yet the way GitHub serves a published
# one, so packaging/install.sh can be run against it unchanged:
#
#   mirror/latest/download/<file>        the candidate (what "latest" resolves to)
#   mirror/download/v<version>/<file>    the candidate, already downloaded here
#   mirror/download/v<previous>/<file>   the previous release, for the update step
#   mirror/install.sh                    this commit's installer
#
# Env: VERSION, PREVIOUS (tag or version, may be empty), ASSETS (glob of the
# previous release's files this leg installs), GH_TOKEN. Exports
# SOUL_PLAYER_INSTALL_URL and SOUL_PLAYER_RELEASES_URL to later steps.
set -euo pipefail
: "${VERSION:?}"
prev=${PREVIOUS#v}
port=8765
root=$PWD/mirror

[ -f "$root/download/v$VERSION/SHA256SUMS" ] || { echo "::error::no candidate at mirror/download/v$VERSION"; exit 1; }
mkdir -p "$root/latest"
ln -sfn "../download/v$VERSION" "$root/latest/download"
if [ -n "$prev" ] && [ "$prev" != "$VERSION" ] && [ -n "${ASSETS:-}" ]; then
  gh release download "v$prev" --repo "$GITHUB_REPOSITORY" --dir "$root/download/v$prev" \
    --pattern "$ASSETS" --pattern SHA256SUMS
fi
cp packaging/install.sh "$root/install.sh"

nohup python3 -m http.server "$port" --bind 127.0.0.1 --directory "$root" >"$RUNNER_TEMP/mirror.log" 2>&1 &
for _ in $(seq 1 20); do
  curl -fsS "http://127.0.0.1:$port/latest/download/latest.json" >/dev/null 2>&1 && break
  sleep 0.5
done
got=$(curl -fsS "http://127.0.0.1:$port/latest/download/latest.json" | jq -r .version)
[ "${got#v}" = "$VERSION" ] || { echo "::error::the candidate's latest.json says $got, expected $VERSION"; exit 1; }
find "$root" -type f | sed "s|$root/||" | sort
{
  echo "SOUL_PLAYER_INSTALL_URL=http://127.0.0.1:$port/install.sh"
  echo "SOUL_PLAYER_RELEASES_URL=http://127.0.0.1:$port"
} >>"$GITHUB_ENV"
