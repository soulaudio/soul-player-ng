# Install

Downloads are on the [releases page](https://github.com/soulaudio/soul-player-ng/releases/latest).

```sh
sudo apt install ./soul-player_*_amd64.deb              # Debian / Ubuntu (arm64 for ARM)
sudo dnf install ./soul-player-*.x86_64.rpm             # Fedora (aarch64 for ARM)
chmod +x soul-player_*.AppImage && ./soul-player_*.AppImage
flatpak install --bundle ./Soul-Player_*.flatpak        # any distro with Flatpak
```

**Windows**: the `-setup.exe` installer.

**macOS**: `Soul-Player_<version>_universal.dmg`, one download that runs natively on
every Mac (Apple silicon and Intel). Drag it to Applications; opened straight from the
disk image it offers to move itself.

Or with Homebrew, from the [soulaudio/tap](https://github.com/soulaudio/homebrew-tap):

```sh
brew tap soulaudio/tap && brew trust soulaudio/tap
brew install --cask soul-player
brew uninstall --zap --cask soul-player   # removes the app and its settings, library index, logs
```

The cask is published by the release itself, through a PR the tap's CI has to pass
(install, launch, uninstall on a fresh Mac). The app updates itself in place, and
`brew upgrade` picks up new versions as well.

## Linux package formats

Four formats, for x86_64 and aarch64:

| Format | Works on | Updates via |
|---|---|---|
| `.deb` | Debian 12+, Ubuntu 22.04+ | `apt upgrade` |
| `.rpm` | Fedora, openSUSE, RHEL 10+ | `dnf upgrade` |
| `.AppImage` | anything with glibc 2.35+ | the app itself |
| Flatpak | anything with Flatpak | re-install the bundle |

### Which ones self-update

Only the AppImage updates itself. The `.deb` and `.rpm` are owned by their package
manager, so the app does not offer an in-app update there: it would have to overwrite a
root-owned file, which cannot work.

The Flatpak is a **self-hosted bundle**, not a Flathub app: it is one file with no
repository behind it, and the in-app updater is off under Flatpak for the same
read-only-`/app` reason. So a new version means downloading the next release's
`.flatpak` and installing it the same way. Flathub would fix that, and is not a one-step
submission; `app/PACKAGING.md` says what it needs.

### Flatpak and bit-perfect output

Bit-perfect exclusive output needs direct access to the sound card. Under Flatpak that
is the `--device=all` permission; revoking it in Flatseal leaves shared playback working
and only disables exclusive mode.

## The glibc floor

The floor is **2.35**. The binary also references two glibc 2.39 symbols (`pidfd_spawnp`,
`pidfd_getpid`, from Rust's process spawning), but those are *weak* (glibc falls back
when they are absent), so 2.35 is the real requirement. Both packages declare it, so an
older system is refused rather than left with something that cannot start.

**RHEL 9 is below the floor** (glibc 2.34); RHEL 10 is fine.

Verified by installing the package and rendering a frame on each of:

| distro | glibc | |
|---|---|---|
| Ubuntu 22.04 | 2.35 | `.deb` |
| Debian 12 | 2.36 | `.deb` |
| Ubuntu 24.04 | 2.39 | `.deb` |
| Fedora 45 | 2.42 | `.rpm` |
| openSUSE Tumbleweed | 2.42 | `.rpm` |

Debian 11 (2.31) and RHEL 9 (2.34) are correctly refused by the package manager.
