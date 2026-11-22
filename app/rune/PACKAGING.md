# Packaging, auto-update & releases

Desktop installers are produced with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager)
(the Tauri bundler extracted to run on any binary). Config lives in
`[package.metadata.packager]` in `Cargo.toml`.

## Build an installer locally

```sh
cargo install cargo-packager          # once
cd app/rune
cargo packager --release              # → target/release/soul-player-rune_<ver>_x64-setup.exe
```

The build uses `app/rune`'s pinned nightly (`rust-toolchain.toml`); cargo-packager
downloads `makensis` on first run. Output on Windows is an **NSIS setup `.exe`**
(~8 MB). To also emit the signed updater artifact locally, set the signing env
(see below) before `cargo packager`.

## macOS (.app + .dmg)

```sh
cd app/rune
cargo build --release                 # cargo-packager packages, it doesn't build
cargo packager --release -f app       # → target/release/Soul Player.app
cargo packager --release -f dmg       # → target/release/Soul Player_<ver>_aarch64.dmg
```

The bundle identity (`co.datadir.soulplayer`, category Music) comes from
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

## Auto-update (implemented, free)

The app updates itself with no paid accounts. **This is independent of OS
code-signing.** On launch, release builds check GitHub Releases for a newer
build; if one exists, **Settings → Updates** shows **Install & Restart**, which
downloads, cryptographically verifies, and installs it, then relaunches. (We
flag-then-let-you-apply rather than restarting mid-session, so a playing track is
never yanked away.) Code: `src/update.rs` + `src/views/settings.rs`.

Updates are verified against our **own updater key** — a minisign keypair that
has nothing to do with Apple/Microsoft code-signing:

- **Public key** — embedded in `src/update.rs` (`PUBKEY`). Safe to commit.
- **Private key** — generated with `cargo packager signer generate`, stored in
  `.signing/` (git-ignored) and as a CI secret. Signs every release. Never
  committed.

The keypair already exists in `.signing/soul-player-updater.key{,.pub}` (created
without a password). To rotate it: regenerate, replace `PUBKEY`, and update the
secrets — note that **users on the old key can't auto-update to a build signed
with a new key**, so rotate only with a plan (e.g. a one-time manual download).

### Disable / env

- `SOUL_PLAYER_NO_UPDATE=1` turns the check off (the e2e harness sets it).
- `SOUL_PLAYER_UPDATE_ENDPOINT=<url>` overrides the manifest URL — for a
  self-hosted update server, and how the updater tests point at a local one.
- Debug builds never check (no matching published release).

## Cutting a release

1. **Dry run first** (on the branch with the version bump):
   `gh workflow run release.yml --ref <branch> -f dry_run=true` — builds and runs the full
   per-OS e2e, publishes nothing; the installers stay as run artifacts.
2. Bump `version` in `app/rune/Cargo.toml`, merge to `main`.
3. Tag the merge commit: `git tag v0.4.0 && git push origin v0.4.0`.

`.github/workflows/release.yml` then runs, stage by stage — nothing reaches users
until every earlier stage is green:

| Stage | What it does |
| --- | --- |
| `guard` | tag == `Cargo.toml` version; the commit is on `main` and its `CI success` check passed (waits for it if still running); the tag isn't already published |
| `build` | per platform (Windows x64, macOS Apple silicon, macOS Intel, Linux x64): `cargo build --release --locked`, `cargo packager` + updater signature, the sidecar for `latest.json` (`write-meta.py`); macOS also builds the DMG and runs the static bundle checks (`verify-macos-bundle.sh`: sealed signature, architecture, identity, launch); SHA-256 of every installer |
| `e2e` | on a fresh runner of each OS, with the exact installer that will ship (`.github/scripts/release-e2e.py`): **fresh install** → headless renders + the real app autoplaying a generated library (stays up, logs the right version, starts playback, no panic, quits cleanly) → **uninstall** leaves nothing behind; then **upgrade** from the currently published release (whose updater must pick up the new `latest.json`, served locally) → **rollback** to that release (it must still run on the config the new one wrote) → uninstall |
| `draft` | a draft GitHub Release with the installers, `latest.json` (`gen-latest-json.py`) and `SHA256SUMS`; the uploaded assets are downloaded again and checked against the tested bytes |
| `publish` | undraft + mark latest (the `release` environment — add required reviewers there to make this a manual approval) |
| `verify` | the public `releases/latest/download/latest.json` serves the new version, covers all 4 platforms, and every URL in it downloads the tested bytes |
| `rollback` | if `draft`, `publish` or `verify` fails: a draft is deleted; a published release is demoted to pre-release and the previous release is made latest again, so in-app updaters stop offering it. The tag is kept — fix forward with a new patch version |

Run the e2e locally (it installs into `/Applications` / the registry / `~/Applications`,
so use a VM or a throwaway account):
`python .github/scripts/release-e2e.py --installer <dmg|setup.exe|AppImage> --version X.Y.Z --target-key macos-aarch64`.

The in-app updater reads `…/releases/latest/download/latest.json`; `latest/`
always points at the newest release, so the endpoint never changes.

> **The repo must be public** (or the release assets hosted somewhere public).
> The updater downloads anonymously — a **private** repo returns `404` for
> `releases/latest/download/…`, so updates silently never apply. Verified live:
> `cargo test -p soul-player-rune live_release -- --ignored` fetches the real
> release, downloads the installer, and verifies its signature end to end.

### One-time CI setup (repo secrets)

| Secret | Value |
| --- | --- |
| `UPDATER_PRIVATE_KEY` | the **contents** of `.signing/soul-player-updater.key` |
| `UPDATER_KEY_PASSWORD` | the key's password — **empty** for the current key |

**Manual rollback** of a published release (outside the workflow):
`gh release edit vX.Y.Z --prerelease --latest=false && gh release edit vPREV --latest`.

## Shipping unsigned (what users see, and how to handle it)

We don't pay for OS code-signing yet, so first-run has friction. **Auto-update is
unaffected** — the updater downloads programmatically (no "mark of the web" /
quarantine), so updates after the first install are silent on every OS.

- **Linux (AppImage)** — no gatekeeper. `chmod +x Soul-Player*.AppImage && ./…`.
  Clean; ship as-is.
- **Windows (NSIS `.exe`)** — SmartScreen shows *"Windows protected your PC /
  unknown publisher"* on the **first** manual download. Users click **More info
  → Run anyway**. Document this on the download page. Later auto-updates don't
  re-trigger it.
- **macOS (unsigned `.app`)** — Gatekeeper **blocks** first launch, and macOS 15
  removed the right-click-Open shortcut. Users open **System Settings → Privacy
  & Security → Open Anyway**, or run
  `xattr -dr com.apple.quarantine "/Applications/Soul Player.app"`. Treat macOS
  as an advanced channel until notarized.

This is how other small/open-source apps ship without signing — a documented
"Run anyway" / "Open Anyway" step on the download page.

## Deferred: OS code-signing (costs money, not required for updates)

Removes the first-run warnings above. Independent of the (working) auto-updater.

- **Windows Authenticode** — e.g. Azure Trusted Signing (~$10/mo) or an EV cert.
  Sign the `.exe` in CI before upload.
- **macOS Developer ID + notarization** — Apple Developer Program ($99/yr);
  `codesign` + `notarytool` in CI. Enables the `dmg` format cleanly.
- **Other formats** — add `"wix"` (`.msi`), `"dmg"`, `"deb"` to the per-OS
  `-f` list once their signing is in place.
