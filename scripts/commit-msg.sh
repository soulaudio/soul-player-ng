#!/bin/sh
# Commit-message policy: Conventional Commits + no AI / attribution references.
# Invoked by lefthook's commit-msg hook: `sh scripts/commit-msg.sh {1}`.
# POSIX sh so it behaves the same on Linux, macOS, and Git Bash on Windows.
set -eu

msg_file="$1"

# Keep only what git itself stores: drop the verbose-diff scissors section and
# comment (#) lines.
body=$(sed -e '/^# ------------------------ >8/,$d' -e '/^#/d' "$msg_file")

# 1. No AI / attribution anywhere (case-insensitive). "ai" only as a whole word
#    so "AIFF", "detail", "chain" do not trip it.
if printf '%s' "$body" | grep -iqE 'claude|anthropic|co-?authored|generated with|🤖|(^|[^[:alnum:]])ai([^[:alnum:]]|$)'; then
	echo "commit message contains a forbidden reference (Claude / Anthropic / AI / co-author)." >&2
	echo "Project policy: the history must stay free of tool attribution. Remove it and retry." >&2
	exit 1
fi

# 2. Conventional Commits subject — first non-empty line, exempting the standard
#    machine-generated subjects (merge, revert, autosquash).
subject=$(printf '%s\n' "$body" | sed '/^[[:space:]]*$/d' | head -n1)
case "$subject" in
	"Merge "* | "Revert "* | "fixup! "* | "squash! "* | "amend! "*) exit 0 ;;
esac
if printf '%s' "$subject" | grep -qE '^(feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(\([^)]+\))?!?: .+'; then
	exit 0
fi

echo "commit subject is not a valid Conventional Commit:" >&2
echo "  $subject" >&2
echo "Expected: <type>(<scope>)!: <description>" >&2
echo "Types: feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert" >&2
exit 1
