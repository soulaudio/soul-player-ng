"""User-facing release notes for the in-app updater (latest.json "notes").

Settings → Updates shows these under "What's new", so they list what a listener
notices: new features, fixes and speedups since the previous release tag, taken
from the Conventional Commit subjects. CI, tests, docs, chores and refactors are
left out. The PR suffix "(#123)" and the scope are dropped, and long lists are
capped with "and N more".

    python release-notes.py --tag v0.4.1 [--max 12]       # prints the notes
    python release-notes.py --selftest
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys

SECTIONS = [("feat", "New"), ("fix", "Fixed"), ("perf", "Faster")]
SUBJECT = re.compile(r"^(?P<type>[a-z]+)(?:\((?P<scope>[^)]*)\))?(?P<bang>!)?: (?P<desc>.+)$")
PR_SUFFIX = re.compile(r"\s*\(#\d+\)\s*$")
# A fix to the release pipeline or the test harness is nothing a listener notices.
INTERNAL_SCOPES = {"ci", "e2e", "release", "release-e2e", "test", "tests"}


def previous_tag(tag: str) -> str | None:
    """The release tag before `tag` (by version order), or None for the first."""
    tags = subprocess.run(["git", "tag", "--list", "v*", "--sort=-v:refname"],
                          capture_output=True, text=True, check=True).stdout.split()
    tags = [t for t in tags if re.fullmatch(r"v\d+\.\d+\.\d+", t)]
    if tag in tags:
        later = tags[tags.index(tag) + 1:]
        return later[0] if later else None
    return tags[0] if tags else None


def subjects(tag: str) -> list[str]:
    # A rehearsal runs before its tag exists: describe the checked-out commit.
    end = tag if subprocess.run(["git", "rev-parse", "-q", "--verify", f"refs/tags/{tag}"],
                                capture_output=True).returncode == 0 else "HEAD"
    prev = previous_tag(tag)
    rng = f"{prev}..{end}" if prev else end
    out = subprocess.run(["git", "log", "--no-merges", "--format=%s", rng],
                         capture_output=True, text=True, check=True).stdout
    return [s for s in out.splitlines() if s.strip()]


def notes(subs: list[str], limit: int = 12) -> str:
    grouped: dict[str, list[str]] = {t: [] for t, _ in SECTIONS}
    for s in subs:
        m = SUBJECT.match(s.strip())
        if not m or m["type"] not in grouped or (m["scope"] or "") in INTERNAL_SCOPES:
            continue
        desc = PR_SUFFIX.sub("", m["desc"]).strip()
        desc = desc[:1].upper() + desc[1:]
        if desc not in grouped[m["type"]]:
            grouped[m["type"]].append(desc)
    lines: list[str] = []
    shown = 0
    total = sum(len(v) for v in grouped.values())
    for t, title in SECTIONS:
        items = grouped[t][: max(0, limit - shown)]
        if not items:
            continue
        lines.append(title)
        lines += [f"• {i}" for i in items]
        shown += len(items)
    if total > shown:
        lines.append(f"…and {total - shown} more")
    return "\n".join(lines) if lines else "Maintenance and stability improvements."


def selftest() -> None:
    subs = [
        "feat(update): Settings → Updates page with scheduled checks (#340)",
        "fix(linux): make the Next session e2e deterministic (#316)",
        "ci: bump actions",
        "test(release-e2e): mark stale via rename",
        "fix(release-e2e): install the AppImage by rename (#352)",
        "perf(output): pre-warm the audio device at startup (#330)",
        "feat!: drop the old config format",
        "Merge branch 'main'",
    ]
    got = notes(subs)
    want = ("New\n• Settings → Updates page with scheduled checks\n• Drop the old config format\n"
            "Fixed\n• Make the Next session e2e deterministic\n"
            "Faster\n• Pre-warm the audio device at startup")
    assert got == want, got
    capped = notes([f"fix: thing {i}" for i in range(20)], limit=3)
    assert capped.endswith("…and 17 more") and capped.count("•") == 3, capped
    assert notes(["ci: x", "docs: y"]) == "Maintenance and stability improvements."
    print("release-notes selftest ok")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag")
    ap.add_argument("--max", type=int, default=12)
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.selftest:
        selftest()
        return
    if not a.tag:
        sys.exit("--tag is required")
    print(notes(subjects(a.tag), a.max))


if __name__ == "__main__":
    main()
