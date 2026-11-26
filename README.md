<!-- Theme-aware mark via GitHub's #gh-*-mode-only fragments on same-repo (markdown)
     images: GitHub hides the non-matching one with CSS keyed on its OWN theme
     (data-color-mode), so it also works in fingerprint-resistant browsers (LibreWolf,
     Tor) that force prefers-color-scheme to light. Same-repo images are proxied, so
     there is no muted fallback tile and the hide actually applies (an absolute URL in
     an HTML <img> gets neither). The marks are pre-sized (markdown can't resize). -->
![Soul Player](docs/soul-mark-dark.png#gh-dark-mode-only)
![Soul Player](docs/soul-mark-light.png#gh-light-mode-only)

# Soul Player

A desktop music player for people who still keep their own files.

![The Albums view](docs/screenshots/albums-dark.png)

## Why

- **Your library stays yours.** No account, no cloud, no tracking. The only thing
  the app contacts on its own is the update check, and you can switch that off.
- **It plays the file, not an approximation.** Gapless, no hidden resampling, and
  exclusive access to the sound card where the platform allows it.
- **Native, not a browser in a window.** The interface is drawn on the GPU by
  Rust, over an audio engine written from scratch.
- **Free, and staying that way.** Apache 2.0. No subscription, no paid tier.

Watched folders, playlists, search, cover art, an equaliser and DSP chain,
ReplayGain, light and dark themes, six languages, media keys. WAV, FLAC, MP3,
M4A (ALAC), OGG, Opus, AIFF, WavPack, CAF, MKA, WebM, and DSD.

Windows, macOS, and Linux. Phones are out of scope.

## Install

From the [releases page](https://github.com/soulaudio/soul-player-ng/releases/latest):
a `-setup.exe` for Windows, a universal `.dmg` for macOS, and `.deb`, `.rpm`,
`.AppImage` or Flatpak for Linux (x86_64 and aarch64, glibc 2.35+).

```sh
sudo apt install ./soul-player_*_amd64.deb              # Debian / Ubuntu
sudo dnf install ./soul-player-*.x86_64.rpm             # Fedora / openSUSE
chmod +x soul-player_*.AppImage && ./soul-player_*.AppImage
flatpak install --bundle ./Soul-Player_*.flatpak        # any distro with Flatpak
```

On macOS, Homebrew works too:

```sh
brew tap soulaudio/tap && brew trust soulaudio/tap
brew install --cask soul-player
```

Details, including which formats update themselves, are in
[docs/INSTALL.md](docs/INSTALL.md).

## Self-hosting

`soul-server` serves your library to the player over the network, sending the
original file bytes, so a remote track is as bit-perfect as a local one.
[docs/SERVER.md](docs/SERVER.md) has the setup.

## Build

All Rust. The UI head (`app/`) is its own workspace on a pinned nightly;
everything else is stable.

```sh
cargo test                                 # backend + server
cd app && cargo build --release            # the player
```

Linux needs a few system libraries, listed with the Nix shells and the test
suites in [docs/DEVELOP.md](docs/DEVELOP.md).

| Doc | What it covers |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the layers fit together |
| [docs/DEVELOP.md](docs/DEVELOP.md) | Toolchains, system deps, Nix, tests |
| [docs/SERVER.md](docs/SERVER.md) | Running Soul Server, and its wire protocol |
| [docs/INSTALL.md](docs/INSTALL.md) | Package formats, glibc floor, self-update |
| [app/PACKAGING.md](app/PACKAGING.md) | Installers, signing, auto-update, releases |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to propose a change |
| [SECURITY.md](SECURITY.md) | Reporting a vulnerability |

## License

[Apache 2.0](LICENSE): free to use, modify, and redistribute, including in
commercial and closed-source products. Copyright © 2026 datadir s. r. o.
(Sebastián Stupák). See [LICENSE](LICENSE) and [NOTICE](NOTICE).

We ship this in the open because we can get things wrong, and we'd rather you
could read it, fix it, and build on it than take our word for it. Fork it, learn
from it, make something better.

If you build something on Soul Audio, we'd love a visible mention somewhere users
can see it:

> Powered by **[Soul Audio](https://github.com/soulaudio)**

A courtesy we appreciate, not a license condition. "Soul Audio" and the logo are
trademarks of datadir s. r. o. Please use them to credit us, not in a way that
implies we endorse your product.
