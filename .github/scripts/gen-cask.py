"""The Homebrew cask for a release: `brew install --cask soulaudio/tap/soul-player`.

It installs the universal DMG (one URL for every Mac, which is what a cask
wants). `auto_updates true` because the app updates itself in place, so
`brew upgrade` leaves it alone instead of fighting the in-app updater. `zap`
lists exactly the paths the in-app uninstall removes on macOS
(`app/src/uninstall.rs`), so `brew uninstall --zap` and Settings ->
About -> Uninstall leave the same nothing behind.

    python gen-cask.py --version 0.4.4 --sha256 <universal dmg sha> --out soul-player.rb
    python gen-cask.py ... --url file:///path/to/Soul-Player_0.4.4_universal.dmg   # CD test
    python gen-cask.py --selftest
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from pathlib import Path

REPO = "soulaudio/soul-player-ng"
BUNDLE_ID = "co.soulaudio.soulplayer"

# Mirrors `uninstall::data_paths` on macOS. The release e2e checks a real
# session's files are all gone after `brew uninstall --zap`.
ZAP = [
    "~/Library/Application Support/soul-player",
    f"~/Library/Caches/{BUNDLE_ID}",
    "~/Library/Caches/soul-player",
    f"~/Library/HTTPStorages/{BUNDLE_ID}",
    "~/Library/Logs/Soul Player",
    f"~/Library/Preferences/{BUNDLE_ID}.plist",
    f"~/Library/Saved Application State/{BUNDLE_ID}.savedState",
]


def cask(version: str, sha256: str, url: str | None = None) -> str:
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError(f"version {version!r}")
    if not re.fullmatch(r"[0-9a-f]{64}", sha256):
        raise ValueError(f"sha256 {sha256!r}")
    url = url or (f"https://github.com/{REPO}/releases/download/"
                  'v#{version}/Soul-Player_#{version}_universal.dmg')
    zap = "".join(f'    "{p}",\n' for p in ZAP)
    return f'''cask "soul-player" do
  version "{version}"
  sha256 "{sha256}"

  url "{url}"
  name "Soul Player"
  desc "Gapless, bit-perfect music player for local and remote libraries"
  homepage "https://github.com/{REPO}"

  livecheck do
    url :url
    strategy :github_latest
  end

  auto_updates true
  depends_on :macos

  app "Soul Player.app"

  uninstall quit: "{BUNDLE_ID}"

  zap trash: [
{zap}  ]

  caveats <<~EOS
    Soul Player is not notarized by Apple yet. If macOS blocks the first launch,
    open System Settings -> Privacy & Security and click "Open Anyway".
    Soul Player updates itself; `brew upgrade` leaves it alone.
  EOS
end
'''


def selftest() -> None:
    c = cask("1.2.3", "a" * 64)
    assert 'version "1.2.3"' in c and 'sha256 "' + "a" * 64 + '"' in c
    assert "releases/download/v#{version}/Soul-Player_#{version}_universal.dmg" in c
    assert all(f'"{p}"' in c for p in ZAP)
    assert 'url "file:///x.dmg"' in cask("1.2.3", "b" * 64, "file:///x.dmg")
    for bad in (("1.2", "a" * 64), ("1.2.3", "nope")):
        try:
            cask(*bad)
        except ValueError:
            continue
        raise AssertionError(f"accepted {bad}")
    print("gen-cask selftest ok")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version")
    ap.add_argument("--sha256", help="of the universal DMG")
    ap.add_argument("--dmg", type=Path, help="compute --sha256 from this file")
    ap.add_argument("--url", help="override the download URL (CD test: file://…)")
    ap.add_argument("--out", type=Path)
    ap.add_argument("--selftest", action="store_true")
    a = ap.parse_args()
    if a.selftest:
        selftest()
        return
    sha = a.sha256 or (hashlib.sha256(a.dmg.read_bytes()).hexdigest() if a.dmg else None)
    if not (a.version and sha):
        sys.exit("--version and --sha256 (or --dmg) are required")
    text = cask(a.version, sha, a.url)
    if a.out:
        a.out.write_text(text, encoding="utf-8")
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
