#!/usr/bin/env python3
"""Keep the AppStream `<releases>` list current, from the same notes the updater shows.

Software centres (GNOME Software, KDE Discover) render `<releases>` as the
app's "What's new". Nothing regenerates it, so it went stale immediately: the
metainfo still advertised 0.4.0 while 0.4.5 was published, meaning every Flatpak
and Flathub user read release notes five versions old. `--check` makes that a
build failure instead of something nobody notices.

The notes come from `.github/scripts/release-notes.py`, which is already the
source for `latest.json`'s "What's new" — so the in-app updater, the release
body and the software centres cannot disagree about what changed.

Edits the file as TEXT rather than through ElementTree on purpose: the metainfo
carries explanatory comments and hand-wrapped prose, and a reparse-and-write
would silently reformat the whole document and drop every comment.

  # add or replace the entry for a version (idempotent)
  metainfo-release.py --version 0.4.5 --date 2026-11-24 --notes-from-tag v0.4.5

  # fail unless an entry for this version exists (what CI runs)
  metainfo-release.py --check 0.4.5
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
METAINFO = HERE / "co.soulaudio.soulplayer.metainfo.xml"
NOTES_SCRIPT = HERE / "../../.github/scripts/release-notes.py"


# The updater's "What's new" is capped at 12 items and ends with "…and N more"
# because that panel is small. A software centre shows the whole list, so ask
# for enough that the cap never bites — and the synthetic tail line is dropped
# below in any case, since `appstreamcli` reads its leading ellipsis as an
# uncapitalised first word (`description-first-word-not-capitalized`).
NOTES_MAX = 40


def notes_for_tag(tag: str) -> str:
    """The same text `latest.json` carries, so the two cannot drift."""
    out = subprocess.run(
        [sys.executable, str(NOTES_SCRIPT.resolve()), "--tag", tag, "--max", str(NOTES_MAX)],
        capture_output=True,
        text=True,
        check=True,
    )
    return out.stdout.strip()


def to_appstream(notes: str) -> str:
    """Turn the generator's "Heading / • item" text into AppStream description XML.

    AppStream accepts only a small subset here — `<p>`, `<ul>`, `<li>` — and
    `appstreamcli validate` rejects anything else, so headings become paragraphs
    and bullets become list items rather than being passed through verbatim.
    """
    lines: list[str] = []
    bullets: list[str] = []

    def flush() -> None:
        if bullets:
            lines.append("        <ul>")
            lines.extend(f"          <li>{b}</li>" for b in bullets)
            lines.append("        </ul>")
            bullets.clear()

    for raw in notes.splitlines():
        line = raw.strip()
        if not line:
            continue
        # "…and N more" is a space-saving affordance for the in-app panel, not a
        # change in its own right; it also trips appstreamcli's capitalisation
        # check. A software centre gets the full list instead.
        if line.startswith("…and ") and line.endswith(" more"):
            continue
        if line.startswith("•"):
            bullets.append(escape(line.lstrip("•").strip()))
        else:
            flush()
            lines.append(f"        <p>{escape(line)}</p>")
    flush()
    return "\n".join(lines)


def escape(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def entry(version: str, date: str, body: str) -> str:
    return (
        f'    <release version="{version}" date="{date}">\n'
        f"      <description>\n{body}\n      </description>\n"
        f"    </release>"
    )


def has_version(text: str, version: str) -> bool:
    return re.search(rf'<release\s+version="{re.escape(version)}"', text) is not None


def upsert(text: str, version: str, date: str, body: str) -> str:
    new = entry(version, date, body)
    # Replace an existing entry for this version wherever it sits, so re-running
    # after editing the notes updates in place instead of duplicating.
    pattern = re.compile(
        rf'[ \t]*<release\s+version="{re.escape(version)}".*?</release>',
        re.DOTALL,
    )
    if pattern.search(text):
        return pattern.sub(new, text, count=1)
    # Newest first: AppStream has no ordering requirement, but every tool and
    # every reader treats the first entry as the latest.
    return text.replace("  <releases>\n", f"  <releases>\n{new}\n", 1)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", metavar="VERSION", help="exit non-zero unless this version has an entry")
    ap.add_argument("--version")
    ap.add_argument("--date", help="YYYY-MM-DD")
    ap.add_argument("--notes-from-tag", help="take notes from release-notes.py for this tag")
    ap.add_argument("--notes", help="literal notes text ('-' for stdin)")
    ap.add_argument("--file", type=Path, default=METAINFO)
    a = ap.parse_args()

    text = a.file.read_text(encoding="utf-8")

    if a.check:
        if has_version(text, a.check):
            print(f"metainfo has a <release> for {a.check}")
            return 0
        latest = re.search(r'<release\s+version="([^"]+)"', text)
        print(
            f"::error::{a.file.name} has no <release version=\"{a.check}\"> entry "
            f"(newest is {latest.group(1) if latest else 'none'}). Software centres "
            f"show this list as the app's release notes, so without an entry they "
            f"advertise an older version's changes. Add one with:\n"
            f"  packaging/flatpak/metainfo-release.py --version {a.check} "
            f"--date $(date -u +%Y-%m-%d) --notes-from-tag v{a.check}",
            file=sys.stderr,
        )
        return 1

    if not (a.version and a.date):
        ap.error("--version and --date are required unless --check is used")

    if a.notes_from_tag:
        notes = notes_for_tag(a.notes_from_tag)
    elif a.notes == "-":
        notes = sys.stdin.read()
    elif a.notes:
        notes = a.notes
    else:
        ap.error("one of --notes-from-tag / --notes is required")

    body = to_appstream(notes)
    if not body.strip():
        print("::error::the generated notes are empty", file=sys.stderr)
        return 1

    out = upsert(text, a.version, a.date, body)
    if out == text:
        print(f"no change ({a.version} already matches)")
        return 0
    a.file.write_text(out, encoding="utf-8")
    print(f"metainfo: wrote <release version=\"{a.version}\" date=\"{a.date}\">")
    return 0


if __name__ == "__main__":
    sys.exit(main())
