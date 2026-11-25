#!/bin/sh
# House style: no em dashes in prose. Commas where one wrapped a parenthetical
# or joined clauses, a colon where it introduced an explanation.
#
# Enforced because it does not hold on its own: the whole repo was swept once,
# and a PR merged three days later put two back. A rule nothing checks is a
# rule that decays.
#
# Three uses are allowed, and they are all the dash as a GLYPH rather than as
# punctuation:
#   "—"                    the "no value" placeholder in settings / signal-path
#                          / titlebar cells
#   {title} — {artist}     the now-playing separator
#   update-fixture.bin     a test fixture whose bytes are pinned by checksum
#
# Run with no arguments to check the whole tree.
set -eu

offenders=$(
	git grep -I -n -- '—' \
		| grep -v '"—"' \
		| grep -v 'ti} — {artist' \
		| grep -v '^app/tests/fixtures/update-fixture.bin:' \
		| grep -v '^scripts/check-em-dashes.sh:' \
		|| true
)

if [ -n "$offenders" ]; then
	echo "em dashes found. Use a comma, a colon, or parentheses instead:" >&2
	echo "$offenders" >&2
	echo >&2
	echo "If one is a deliberate glyph rather than punctuation, add it to the" >&2
	echo "allowed list in scripts/check-em-dashes.sh and say why." >&2
	exit 1
fi
