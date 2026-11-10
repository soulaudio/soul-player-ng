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

1. Bump `version` in `app/rune/Cargo.toml`, commit.
2. Tag it: `git tag v0.2.0 && git push origin v0.2.0` (the tag **must** match the
   manifest version — the release workflow's `guard` job fails otherwise).
3. `.github/workflows/release.yml` (tag-triggered, so it doesn't touch the
   day-to-day Actions budget) then, per OS:
   - builds + **signs** the bundle (`cargo packager --release -f <format>`),
   - writes a per-platform sidecar (`.github/scripts/write-meta.py`),
   - and the `publish` job assembles `latest.json`
     (`.github/scripts/gen-latest-json.py`) and uploads the installers +
     manifest to the GitHub Release.

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

Actions is currently disabled repo-wide to conserve quota; the release workflow
only fires on a `v*` tag, so it runs when you enable Actions and push a tag.

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
