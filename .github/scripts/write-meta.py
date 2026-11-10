#!/usr/bin/env python3
"""Write the `<key>.meta.json` sidecar for one packaged platform.

cargo-packager signs exactly one updater artifact per format and drops
`<artifact>.sig` beside it (verified locally: `…_x64-setup.exe` + `.sig`). We
locate that lone `.sig`, derive the artifact by stripping the `.sig` suffix, and
record {target_key, format, artifact-basename, signature}. Deriving from the
`.sig` means this works for nsis / app / appimage without hard-coding any bundle
name. gen-latest-json.py later turns these sidecars into `latest.json`.
"""

import argparse
import json
import sys
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser(description="Write a packaged-platform meta sidecar.")
    ap.add_argument("--out-dir", required=True, help="cargo-packager output dir to scan")
    ap.add_argument("--target-key", required=True, help="e.g. windows-x86_64")
    ap.add_argument("--format", required=True, help="nsis | app | appimage | wix")
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

    meta = {
        "target_key": a.target_key,
        "format": a.format,
        "artifact": artifact.name,
        "signature": sig.read_text(encoding="utf-8").strip(),
    }
    Path(a.meta_out).write_text(json.dumps(meta, indent=2), encoding="utf-8")
    print(f"meta[{a.target_key}] -> {artifact.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
