#!/usr/bin/env bash
# Withdraw a release: the one thing release.yml's `rollback` job does, kept
# here so rollback-drill.yml exercises exactly the same code against a real
# release rather than a copy of it.
#
#   rollback-release.sh <tag> [<previous-tag>]
#
#   no release    nothing to do
#   a draft       deleted (nothing was ever public; the tag, if pushed, stays)
#   published     demoted to pre-release and no longer latest, and <previous>
#                 made latest again, so in-app updaters (which read
#                 releases/latest/download/latest.json) and install.sh stop
#                 offering it. The assets stay, for diagnosis.
#
# Then it checks the result the way users would see it, and exits non-zero if
# the release is still offered. Needs GH_TOKEN with contents: write.
set -euo pipefail
TAG=${1:?usage: rollback-release.sh <tag> [<previous-tag>]}
PREV=${2:-}
REPO=${GITHUB_REPOSITORY:?}
summary() { echo "$*" | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"; }

state=$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq .isDraft 2>/dev/null || echo none)
case "$state" in
  none)
    summary "No release for $TAG, nothing to roll back." ;;
  true)
    gh release delete "$TAG" --repo "$REPO" --yes
    if gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
      echo "::error::the draft $TAG is still there"; exit 1
    fi
    summary "Deleted the draft $TAG." ;;
  false)
    gh release edit "$TAG" --repo "$REPO" --prerelease --latest=false
    if [ -n "$PREV" ] && [ "$PREV" != "$TAG" ]; then
      gh release edit "$PREV" --repo "$REPO" --latest
    fi
    # Verify, the way users resolve "latest": the API, and the redirect the
    # updater and install.sh follow (GitHub can take a moment to move it).
    latest=$(gh release view --repo "$REPO" --json tagName --jq .tagName)
    [ "$latest" != "$TAG" ] || { echo "::error::$TAG is still the latest release"; exit 1; }
    [ -z "$PREV" ] || [ "$latest" = "$PREV" ] || { echo "::error::latest is $latest, expected $PREV"; exit 1; }
    pre=$(gh release view "$TAG" --repo "$REPO" --json isPrerelease --jq .isPrerelease)
    [ "$pre" = true ] || { echo "::error::$TAG is not marked pre-release"; exit 1; }
    if [ -n "$PREV" ]; then
      for _ in $(seq 1 30); do
        served=$(curl -fsSL "https://github.com/$REPO/releases/latest/download/latest.json" | jq -r .version) || served=
        [ "v${served#v}" = "$PREV" ] && break
        sleep 5
      done
      [ "v${served#v}" = "$PREV" ] || { echo "::error::releases/latest/download/latest.json still says ${served:-nothing}, expected $PREV"; exit 1; }
    fi
    summary "Demoted $TAG to pre-release; latest is ${PREV:-unset} again, and latest.json serves it." ;;
esac
