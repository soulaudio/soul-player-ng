#!/usr/bin/env python3
"""Stage one packaged platform's signed artifact and write its meta sidecar.

cargo-packager signs exactly one updater artifact per format and drops
`<artifact>.sig` beside it. We locate that lone `.sig`, derive the artifact by
stripping the suffix, copy it into the staging dir under a **GitHub-safe name**,
and record {target_key, format, staged-name, signature}. gen-latest-json.py
later turns these sidecars into `latest.json`.

The sanitize-and-stage matters: GitHub rewrites release-asset filenames,
replacing spaces and other specials with `.` (so `Soul Player.app.tar.gz`
uploads as `Soul.Player.app.tar.gz`). If we recorded the original name the
manifest URL would 404. By staging under the already-sanitized name, the name we
upload is the name GitHub keeps, so the URL matches. The signature is over the
bytes, not the name, so renaming is safe.
"""

import argparse
import json
import re
import shutil
import sys
from pathlib import Path


def github_safe(name: str) -> str:
    # Mirror GitHub's release-asset sanitization: anything outside
    # [A-Za-z0-9._-] becomes '.'. Leaves our Windows/Linux names untouched.
    return re.sub(r"[^A-Za-z0-9._-]", ".", name)


def main() -> int:
    ap = argparse.ArgumentParser(description="Stage a signed artifact + write its meta.")
    ap.add_argument("--out-dir", required=True, help="cargo-packager output dir to scan")
    ap.add_argument("--target-key", required=True, help="e.g. windows-x86_64")
    ap.add_argument("--format", required=True, help="nsis | app | appimage | wix")
    ap.add_argument("--stage-dir", required=True, help="dir to copy the artifact into")
    ap.add_argument("--meta-out", required=True, help="path to write <key>.meta.json")
    a = ap.parse_args()

    sigs = sorted(Path(a.out_dir).rglob("*.sig"))
    if len(sigs) != 1:
        found = [str(s) for s in sigs]
        print(
            f"error: expected exactly one .sig under {a.out_dir}, found {found}",
            file=sys.stderr,
        )
        return 1
    sig = sigs[0]
    artifact = sig.with_suffix("")  # strip the trailing ".sig"
    if not artifact.exists():
        print(f"error: signed artifact {artifact} is missing", file=sys.stderr)
        return 1

    safe_name = github_safe(artifact.name)
    stage = Path(a.stage_dir)
    stage.mkdir(parents=True, exist_ok=True)
    shutil.copy2(artifact, stage / safe_name)

    meta = {
        "target_key": a.target_key,
        "format": a.format,
        "artifact": safe_name,
        "signature": sig.read_text(encoding="utf-8").strip(),
    }
    Path(a.meta_out).write_text(json.dumps(meta, indent=2), encoding="utf-8")
    print(f"meta[{a.target_key}] staged {artifact.name} -> {safe_name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
