#!/usr/bin/env python3
"""Assemble the cargo-packager-updater manifest (`latest.json`) for a release.

The updater (app/rune/src/update.rs) fetches this file from
`…/releases/latest/download/latest.json` and, if its `version` is newer than the
running app, downloads the matching platform's `url` and verifies it against the
`signature` (which must be the exact contents of the `.sig` cargo-packager
emitted when signing the bundle). Schema per the cargo-packager-updater docs:

    {
      "version":  "0.2.0",                 # semver, leading "v" allowed
      "pub_date": "2026-11-10T12:00:00Z",  # RFC 3339
      "notes":    "…",
      "platforms": {
        "windows-x86_64": {"url": "…", "signature": "…", "format": "nsis"},
        "darwin-aarch64": {"url": "…", "signature": "…", "format": "app"},
        "linux-x86_64":   {"url": "…", "signature": "…", "format": "appimage"}
      }
    }

Platform keys are `OS-ARCH` (OS ∈ linux|macos|windows; note the updater also
accepts `darwin`/`windows` the runner reports — we emit exactly what each
package job wrote). Each package job drops a `<key>.meta.json` sidecar next to
its artifact:

    {"target_key": "windows-x86_64", "format": "nsis",
     "artifact": "Soul Player_0.2.0_x64-setup.exe", "signature": "<sig bytes>"}

This script reads every sidecar in --meta-dir, turns each `artifact` basename
into its GitHub release-asset URL, and writes the assembled manifest. Keeping it
sidecar-driven means a partial release (say Windows only, because the macOS job
was skipped) still produces a valid manifest covering just the platforms built.
"""

import argparse
import json
import sys
from pathlib import Path


def asset_url(repo: str, tag: str, artifact: str) -> str:
    # GitHub serves release assets at this stable path; spaces in the bundle
    # name are percent-encoded so the URL is valid.
    from urllib.parse import quote

    return f"https://github.com/{repo}/releases/download/{tag}/{quote(artifact)}"


def main() -> int:
    ap = argparse.ArgumentParser(description="Generate the updater latest.json.")
    ap.add_argument("--meta-dir", required=True, help="dir of <key>.meta.json sidecars")
    ap.add_argument("--repo", required=True, help="owner/name, e.g. soulaudio/soul-player-ng")
    ap.add_argument("--tag", required=True, help="release tag, e.g. v0.2.0")
    ap.add_argument("--version", required=True, help="semver, e.g. 0.2.0")
    ap.add_argument("--pub-date", required=True, help="RFC 3339, e.g. 2026-11-10T12:00:00Z")
    ap.add_argument("--notes", default="", help="release notes")
    ap.add_argument("--out", required=True, help="output path for latest.json")
    args = ap.parse_args()

    platforms: dict[str, dict[str, str]] = {}
    sidecars = sorted(Path(args.meta_dir).rglob("*.meta.json"))
    if not sidecars:
        print(f"error: no *.meta.json sidecars under {args.meta_dir}", file=sys.stderr)
        return 1

    for sc in sidecars:
        meta = json.loads(sc.read_text(encoding="utf-8"))
        key = meta["target_key"]
        sig = meta["signature"].strip()
        if not sig:
            print(f"error: empty signature in {sc}", file=sys.stderr)
            return 1
        platforms[key] = {
            "url": asset_url(args.repo, args.tag, meta["artifact"]),
            "signature": sig,
            "format": meta["format"],
        }

    manifest = {
        "version": args.version,
        "pub_date": args.pub_date,
        "notes": args.notes,
        "platforms": platforms,
    }
    out = Path(args.out)
    out.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {out} covering: {', '.join(sorted(platforms))}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
