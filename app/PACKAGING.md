# Packaging, auto-update & releases

Desktop installers are produced with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager)
(the Tauri bundler extracted to run on any binary). Config lives in
`[package.metadata.packager]` in `Cargo.toml`.

## Build an installer locally

```sh
cargo install cargo-packager          # once
cd app
cargo packager --release              # → target/release/soul-player_<ver>_x64-setup.exe
```

The build uses `app`'s pinned nightly (`rust-toolchain.toml`); cargo-packager
downloads `makensis` on first run. Output on Windows is an **NSIS setup `.exe`**
(~8 MB). To also emit the signed updater artifact locally, set the signing env
(see below) before `cargo packager`.

## macOS (.app + .dmg)

```sh
cd app
cargo build --release                 # cargo-packager packages, it doesn't build
cargo packager --release -f app       # → target/release/Soul Player.app
cargo packager --release -f dmg       # → target/release/Soul Player_<ver>_aarch64.dmg
```

The bundle identity (`co.soulaudio.soulplayer`, category Music) comes from
`[package.metadata.packager]`. Run the app from the bundle to test anything that depends
on identity: the Now Playing widget's name/icon and notifications (`rune-notify` only
posts from a bundle). Releases ship a DMG per architecture (`aarch64` for Apple silicon,
`x86_64` for Intel) for first installs, and a matching `.app.tar.gz` for the updater.

Not Apple-signed yet: the bundle carries a sealed **ad-hoc** signature
(`[package.metadata.packager.macos] signing-identity = "-"`, no certificate or secret),
so `codesign --verify --deep --strict` passes and the DMG/tarball copies are intact, but
Gatekeeper still blocks the first launch of a download (see *Shipping unsigned*).
Developer ID signing + notarization come later (Apple Developer account).

Check a packaged DMG the way the release does:
`.github/scripts/verify-macos-bundle.sh <version> arm64 "target/release/Soul Player_<ver>_aarch64.dmg"`
(signature, architecture, identity, version, icon, DMG layout, and a headless launch).

## Linux (.deb, .rpm, AppImage, Flatpak)

All four are built by one script, for x86_64 and aarch64:

```sh
packaging/build-linux.sh            # or --skip-build to reuse target/release
```

It strips the binary (a full strip, ~23% off the binary and ~30% off the .deb),
checks the declared glibc floor against what the binary actually requires,
generates the Debian changelog, builds the `.deb`, stages the desktop entry and
icon out of it, and builds the `.rpm` from that same staging. The Flatpak is
built in CI from the same staged files.

Staging through the .deb is deliberate: cargo-packager generates the desktop
entry and icon set from `[package.metadata.packager]`, which is the one place
those values are written down. Re-deriving them per format would give three
chances to disagree about the app's name, category or icon, and an entry that
disagrees with the window's `app_id` silently costs the icon.

### The package name, and what the rename did and did not change

Every Linux package is `soul-player`. The crate stays `soul-player-rune`:
"rune" names this UI head, which means nothing to someone installing a music
player, so it is kept to places only developers see (the crate, `cargo test
-p`, the `nix develop .#rune` shell).

Two different names come from two different places, and conflating them is easy:

| | source | changed by the rename? |
|---|---|---|
| `.deb`/AppImage/NSIS **filename** | main binary's name | **yes** |
| installed binary, desktop entry, MPRIS suffix | main binary's name | **yes** |
| `.deb` **`Package:`** field | kebab-cased `product-name` | no, always `soul-player` |
| `.rpm` package name | `[package.metadata.generate-rpm] name` | no |

So **no package was ever renamed**, and neither the `.deb` nor the `.rpm` needs
`Provides`/`Conflicts`/`Replaces` or `Obsoletes`: do not add them. The
published 0.4.2 `.deb` settles it: the file is
`soul-player-rune_0.4.2_amd64.deb` and inside it is `Package: soul-player`.

What the rename actually does is move `/usr/bin/soul-player-rune` to
`/usr/bin/soul-player` *within* one package, and both package managers drop
paths a new version no longer ships. Verified in containers rather than
reasoned about: install the published package, upgrade to the renamed build,
and exactly one binary and one desktop entry remain (`Unpacking soul-player
over (0.4.2)` on Debian; the same on Fedora with `rpm -U`).

This was originally built the other way round, on the assumption that
`Package:` followed the binary name: a transition apparatus with three control
fields patched into the built `.deb`, plus a gate that hunted for "the last
old-named release". The release workflow's own upgrade check disproved it by
failing on an assertion that the old package name was installed, it never was.
The check remains, asserting the thing that can actually break: that the
previous release's binary path is gone after the upgrade.

### What differs per format

| | identity | updates | notes |
|---|---|---|---|
| `.deb` | binary name | `apt` | `libc6 (>= 2.35)`; `t64` alternatives so one package covers Debian 12 and Ubuntu 24.04 |
| `.rpm` | binary name | `dnf` | SONAME dependencies, so it resolves on Fedora **and** openSUSE |
| AppImage | binary name | **itself** | the only format the in-app updater can replace |
| Flatpak | `$FLATPAK_ID` | `flatpak` | sandboxed; needs `--device=all` for bit-perfect output |

The app resolves its own identity at runtime (`desktop_id()`): normally the
binary name, but `$FLATPAK_ID` inside a Flatpak, because Flatpak installs the
desktop entry as `$FLATPAK_ID.desktop` and grants exactly one MPRIS name,
`org.mpris.MediaPlayer2.$FLATPAK_ID`. Get that wrong and media keys stop
working while everything else looks fine.

Self-update is disabled wherever a package manager owns the install
(`update::manager_owned`): `/usr/bin` is root-owned, so the updater would
download tens of megabytes and fail on a permission error.

### Dependencies

Derived from the built binary, its `NEEDED` entries plus every SONAME it
`dlopen`s: not from guesswork. The dlopened ones never appear in `ldd`, so
they are easy to miss and produce a package that installs cleanly and then
fails at runtime (fontconfig's absence means no text renders at all).

Equally, the app does **not** need gtk3, libayatana-appindicator, libxdo or
libGL: the tray is spoken directly over D-Bus
(`org.kde.StatusNotifierItem` + `com.canonical.dbusmenu`), and wgpu's GL
fallback is not compiled in. Depending on gtk3 would pull ~40 MB of transitive
dependencies in for nothing.

### Verifying a change

Build the packages, then install and RUN them in a container, building is not
evidence:

```sh
podman run --rm -v "$PWD/app/target/release:/w:z" ubuntu:24.04 \
  bash -c 'apt-get update -qq && apt-get install -y -qq /w/*.deb && soul-player --help'
```

The release pipeline does this for real on every platform, and its Flatpak step
starts the sandboxed app and fails unless it owns its MPRIS name.

### Keeping the metainfo current (enforced)

Software centres render the AppStream `<releases>` list as the app's "What's
new". Nothing regenerated it, so it advertised 0.4.0 while 0.4.5 was published: 
valid AppStream the whole time, which is why `appstreamcli validate` never
objected. Two things now stop that:

```sh
# writes/updates the entry, taking its text from the SAME generator that fills
# latest.json, so the updater and the software centres cannot disagree
packaging/flatpak/metainfo-release.py --version 0.4.6 \
  --date $(date -u +%Y-%m-%d) --notes-from-tag v0.4.6
```

* `the_metainfo_lists_the_version_being_shipped` fails unless there is a
  `<release>` entry for `app/Cargo.toml`'s version, so a version bump that
  forgets the notes does not reach main.
* The release workflow runs **`flatpak-builder-lint`** (appstream + manifest)
  fatally. That is Flathub's own linter and it is not the same as
  `appstreamcli validate`: validation asks "is this valid AppStream", the linter
  asks "would Flathub accept this", missing release information, absent OARS
  rating, no developer name, a manifest without `command` or `modules`, network
  access during build. Flathub runs the manifest and repo checks on their own
  infrastructure, so whatever fails here fails there.

### Flathub (manifest built; submission is the remaining step)

Two Flatpak manifests exist, and they differ in exactly one respect:

| | builds from | produces |
|---|---|---|
| `packaging/flatpak/` | the already-tested binary | the `.flatpak` bundle on each release |
| `packaging/flathub/` | **source, offline** | what Flathub's builders compile |

Flathub requires the second shape, *"All source available submissions must be
built entirely from source code"*, with prebuilt `extra-data` reserved for
non-redistributable content, which Apache-2.0 is not. `the_two_flatpak_manifests_agree`
keeps them from diverging on app id, runtime or sandbox permissions, since a
`--device=all` present in one and not the other would mean bit-perfect output
worked for some users and not others.

Because *"there is no network access during the build process"*, everything the
compiler needs is a checksummed source:

* **`cargo-sources.json`**: ~1500 entries generated from `app/Cargo.lock`
  by `flatpak-cargo-generator`, with both git dependencies (`rune`,
  `soul-audio-core`) resolved to the commits the lockfile pins. **Not
  committed**: it is derived entirely from the lockfile, so a committed copy
  only created a second thing to keep in sync, and with Dependabot watching
  `app`, that meant every dependency bump failed CI until someone
  regenerated 440 KB. CI generates it; run
  `packaging/flathub/generate-sources.sh` before submitting, since Flathub's own
  repository does need the file.
* **The toolchain**: the dated nightly tarball from `static.rust-lang.org` per
  architecture, by sha256. Installing it directly rather than via rustup is what
  makes it reproducible: with no rustup shim, `rust-toolchain.toml` is inert and
  the build gets exactly `nightly-2026-11-10`. Do **not** use the `rust-nightly`
  SDK extension: archived September 2026, untouched since May 2024.

`cargo` runs from the build root, not `app`: the generated vendor config
says `directory = "cargo/vendor"`, relative to the working directory.

### What is pinned, and what fails if you move it

The packaging tools are pinned to exact versions, and tests fail if a pin and
the thing it is supposed to match drift apart. `cargo install --locked` does
**not** pin a version (it only respects the installed crate's own lockfile), so
before this the release floated across cargo-packager releases while the tests
described one version's behaviour.

| Pin | Where | Guard |
|---|---|---|
| `cargo-packager 0.11.8` | `release.yml` (two places) | `the_pinned_packager_version_matches_what_the_tests_assume` |
| `cargo-generate-rpm 0.21.1` | `release.yml` | none |
| `nightly-2026-11-10` | `rust-toolchain.toml` **and** the Flathub manifest's archive URLs | `the_flathub_toolchain_matches_the_pinned_nightly` |
| app id, runtime, `finish-args` | both Flatpak manifests | `the_two_flatpak_manifests_agree` |
| desktop entry name/comment | `packaging/flathub/*.desktop` vs `Cargo.toml` | `the_flathub_desktop_entry_matches_the_packaged_one` |
| shipped version | `Cargo.toml` vs the metainfo's `<releases>` | `the_metainfo_lists_the_version_being_shipped` |

Bumping cargo-packager is deliberate work, not a version bump: it decides the
`.deb`'s filename and `Package:` field, which config keys are accepted, and the
NSIS template our preinstall hook depends on. Change `PACKAGER_VERSION` in
`app/tests/packaging_metadata.rs` and re-read those assumptions against the
new source.

The `.github/workflows/flathub.yml` job builds it offline with
`org.flatpak.Builder` (the same tool Flathub uses, carrying the matching
linter), runs `flatpak-builder-lint builddir` and `repo`, then starts the app and
requires it to own its MPRIS name. It runs on PRs that touch the lockfile, the
manifest or the toolchain pin, weekly to catch upstream rot, and on demand: 
**not** on releases, because Flathub compiles on their own infrastructure and at
release time nothing has changed but the tag.

To submit: fork `flathub/flathub` with "copy master only" unchecked, branch from
`new-pr`, add the manifest at the top level named for the app id, and open a PR
against `new-pr` titled `Add co.soulaudio.soulplayer`. The PR must explain the
permissions: `--device=all` is the one they will ask about. The app id is a
reverse-DNS claim on `soulaudio.co`, which must stay reachable over HTTPS;
Flathub may ask for a token at
`https://soulaudio.co/.well-known/org.flathub.VerifiedApps.txt`.

Submit against a tag whose metainfo lists that version, the store shows
`<releases>` as the app's release notes, so submitting an older tag would
advertise stale ones.

## Auto-update (implemented, free)

> **One-time, 0.4.2 → 0.4.3 only (macOS and Windows):** the update installs
> correctly but the app does **not** reopen by itself; it starts normally when
> launched again. The relaunch is done by the build being replaced, using its
> own `current_exe`: and the rename deleted that path. Nothing in the new build
> can change that, because the code performing the relaunch already shipped in
> 0.4.2. `relaunch_target` now resolves the renamed binary beside the old path
> so a future rename cannot repeat it, and the release e2e detects this exact
> situation and verifies the installed build runs when started rather than
> failing the release. The AppImage is unaffected (one file, same path).


The app updates itself with no paid accounts. **This is independent of OS
code-signing.** On launch, release builds check GitHub Releases for a newer
build; if one exists, **Settings → Updates** shows **Install & Restart**, which
downloads, cryptographically verifies, and installs it, then relaunches. (We
flag-then-let-you-apply rather than restarting mid-session, so a playing track is
never yanked away.) Code: `src/update.rs` + `src/views/settings.rs`.

Updates are verified against our **own updater key**, a minisign keypair that
has nothing to do with Apple/Microsoft code-signing:

- **Public key**: embedded in `src/update.rs` (`PUBKEY`). Safe to commit.
- **Private key**: generated with `cargo packager signer generate`, stored in
  `.signing/` (git-ignored) and as a CI secret. Signs every release. Never
  committed.

The keypair already exists in `.signing/soul-player-updater.key{,.pub}` (created
without a password). To rotate it: regenerate, replace `PUBKEY`, and update the
secrets: note that **users on the old key can't auto-update to a build signed
with a new key**, so rotate only with a plan (e.g. a one-time manual download).

### Disable / env

- `SOUL_PLAYER_NO_UPDATE=1` turns the check off (the e2e harness sets it).
- `SOUL_PLAYER_UPDATE_ENDPOINT=<url>` overrides the manifest URL, for a
  self-hosted update server, and how the updater tests point at a local one.
- `SOUL_PLAYER_AUTO_INSTALL_UPDATE=1` installs a found update unattended instead
  of waiting for **Install & Restart** (the release e2e drives real updates with it).
- `SOUL_PLAYER_UPDATE_INTERVAL_SECS=<n>` overrides the re-check interval (tests).

### Settings → Updates

The policy follows Sparkle / VS Code / GitHub Desktop:

- **Check for updates automatically** (default **on**, `update_auto_check` in
  `config.toml`): at launch, then every 12 h while the app is open (±10 % jitter so
  clients don't check in lockstep; a failed check retries after 30 min; never more
  often than hourly). A found release is offered on the page and announced once
  with a notification.
- **Install updates automatically** (default **off**, `update_auto_install`):
  download + verify in the background and install when the user next quits, the
  app is never restarted out from under a playing track, and does not reopen.
- **Check Now** and **Install & Restart** on the page, with the release notes
  from `latest.json` and the time of the last check.

"Install & Restart" downloads and verifies the update, quits the app normally
(quit hooks run, so the next launch reports no crash), then installs it once the
run loop has returned: Windows hands over to the NSIS installer in passive mode,
which restarts the app (`/R`); macOS swaps the `.app` bundle and Linux the
AppImage in place, and the app starts the new build itself.
- Debug builds never check (no matching published release).

## Cutting a release

1. **Bump `version` in `app/Cargo.toml` first**, before the dry run, not after.
2. **Dry run** on that branch:
   `gh workflow run release.yml --ref <branch> -f dry_run=true`, builds and runs the full
   per-OS e2e, publishes nothing; the installers stay as run artifacts.
3. Merge to `main`, then tag the merge commit:
   `git tag v0.4.3 && git push origin v0.4.3`.

The order matters more than it looks. While the built version **equals** the
published one, every upgrade path quietly tests nothing, each in its own way:

| | with an equal version |
|---|---|
| `apt-get install` | "already the newest version", does nothing at all |
| NSIS installer | takes its `reinst_done` branch and **skips the uninstall** |
| in-app updater | sees no newer version to offer |

None of those announce themselves as skipped; two of them look like a clean
pass. A rename shipped this way would leave the old binary beside the new one
on Windows and nobody would learn it from a green dry run, which is exactly
what happened while preparing the `soul-player` rename, three times, before the
version was bumped.

`.github/workflows/release.yml` then runs, stage by stage, nothing reaches users
until every earlier stage is green:

| Stage | What it does |
| --- | --- |
| `guard` | tag == `Cargo.toml` version; the commit is on `main` and its `CI success` check passed (waits for it if still running); the tag isn't already published |
| `build` | per platform (Windows x64, macOS Apple silicon, macOS Intel, Linux x64): `cargo build --release --locked`, `cargo packager` + updater signature, the sidecar for `latest.json` (`write-meta.py`); macOS also builds the DMG and runs the static bundle checks (`verify-macos-bundle.sh`: sealed signature, architecture, identity, launch); SHA-256 of every installer |
| `e2e` | on a fresh runner of each OS, with the exact installer that will ship (`.github/scripts/release-e2e.py`): **fresh install** → headless renders + the real app autoplaying a generated library (stays up, logs the right version, starts playback, no panic, quits cleanly) → **auto-update**: the installed app, pointed at a local server offering a newer version whose download is this release's own signed updater artifact, must download + verify it, quit, install it through the OS's real mechanism and come back up on the replaced files with no crash reported → **update settings** (set through the real `config.toml`): a release published while the app runs is found by a scheduled re-check and not installed on its own; with automatic checks off there is no update traffic at all; with automatic install on it downloads in the background, installs nothing mid-session, installs on quit and stays closed → **uninstall** leaves nothing behind; then **upgrade** from the currently published release (whose updater must pick up the new `latest.json`, served locally, and, for releases after 0.3.0, install it itself) → **rollback** to that release (it must still run on the config the new one wrote) → uninstall |
| `draft` | a draft GitHub Release with the installers, `latest.json` (`gen-latest-json.py`) and `SHA256SUMS`; the uploaded assets are downloaded again and checked against the tested bytes |
| `publish` | undraft + mark latest (the `release` environment, add required reviewers there to make this a manual approval) |
| `verify` | the public `releases/latest/download/latest.json` serves the new version, covers all 4 platforms, and every URL in it downloads the tested bytes |
| `rollback` | if `draft`, `publish` or `verify` fails: a draft is deleted; a published release is demoted to pre-release and the previous release is made latest again, so in-app updaters stop offering it. The tag is kept, fix forward with a new patch version |

Run the e2e locally (it installs into `/Applications` / the registry / `~/Applications`,
so use a VM or a throwaway account):
`python .github/scripts/release-e2e.py --installer <dmg|setup.exe|AppImage> --version X.Y.Z --target-key macos-aarch64`
(add `--update-meta <key>.meta.json` for the auto-update leg; on macOS
`SOUL_E2E_APPS_DIR=<dir>` installs somewhere other than `/Applications`).

The in-app updater reads `…/releases/latest/download/latest.json`; `latest/`
always points at the newest release, so the endpoint never changes.

> **The repo must be public** (or the release assets hosted somewhere public).
> The updater downloads anonymously, a **private** repo returns `404` for
> `releases/latest/download/…`, so updates silently never apply. Verified live:
> `cargo test -p soul-player-rune live_release -- --ignored` fetches the real
> release, downloads the installer, and verifies its signature end to end.

### One-time CI setup (repo secrets)

| Secret | Value |
| --- | --- |
| `UPDATER_PRIVATE_KEY` | the **contents** of `.signing/soul-player-updater.key` |
| `UPDATER_KEY_PASSWORD` | the key's password, **empty** for the current key |

**Manual rollback** of a published release (outside the workflow):
`gh release edit vX.Y.Z --prerelease --latest=false && gh release edit vPREV --latest`.

## Shipping unsigned (what users see, and how to handle it)

We don't pay for OS code-signing yet, so first-run has friction. **Auto-update is
unaffected**: the updater downloads programmatically (no "mark of the web" /
quarantine), so updates after the first install are silent on every OS.

- **Linux (AppImage)**: no gatekeeper. `chmod +x Soul-Player*.AppImage && ./…`.
  Clean; ship as-is.
- **Windows (NSIS `.exe`)**, SmartScreen shows *"Windows protected your PC /
  unknown publisher"* on the **first** manual download. Users click **More info
  → Run anyway**. Document this on the download page. Later auto-updates don't
  re-trigger it.
- **macOS (unsigned `.app`)**, Gatekeeper **blocks** first launch, and macOS 15
  removed the right-click-Open shortcut. Users open **System Settings → Privacy
  & Security → Open Anyway**, or run
  `xattr -dr com.apple.quarantine "/Applications/Soul Player.app"`. Treat macOS
  as an advanced channel until notarized.

This is how other small/open-source apps ship without signing, a documented
"Run anyway" / "Open Anyway" step on the download page.

## Deferred: OS code-signing (costs money, not required for updates)

Removes the first-run warnings above. Independent of the (working) auto-updater.

- **Windows Authenticode**: e.g. Azure Trusted Signing (~$10/mo) or an EV cert.
  Sign the `.exe` in CI before upload.
- **macOS Developer ID + notarization**, Apple Developer Program ($99/yr);
  `codesign` + `notarytool` in CI. Enables the `dmg` format cleanly.
- **Other formats**: `"wix"` (`.msi`) is the remaining one worth adding once
  its signing is in place. The Linux formats already ship; see below.
