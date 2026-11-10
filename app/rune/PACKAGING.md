# Packaging the Rune head

Desktop installers are produced with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager)
(the Tauri bundler extracted to run on any binary). Config lives in
`[package.metadata.packager]` in `Cargo.toml`.

## Build an installer (local)

```sh
cargo install cargo-packager          # once
cd app/rune
cargo packager --release              # → target/release/soul-player-rune_<ver>_x64-setup.exe
```

The build uses `app/rune`'s pinned nightly (`rust-toolchain.toml`); cargo-packager
downloads `makensis` on first run. Output is an **NSIS setup `.exe`** for Windows.

## Status (Phase 3 of the Rune migration)

**Done:** an **unsigned** Windows installer. It runs, but SmartScreen shows an
"unknown publisher" warning (expected for unsigned apps).

**Deferred** (needs signing accounts — ~$220/yr; see
`docs/superpowers/specs/2026-11-10-rune-head-migration.md` §Phase 3):

- **OS code-signing** — Windows Authenticode (Azure Trusted Signing) + macOS
  Developer ID + notarization. Removes the SmartScreen/Gatekeeper warnings.
- **Auto-update** — `cargo-packager-updater` (asymmetric-key-signed updates,
  GitHub Releases backend). The public key is embedded in the app; the private
  key signs releases in CI. Depends on signing, so it ships with it.
- **Other-OS formats** — add `"wix"` (Windows `.msi`), `"dmg"` (macOS),
  `"deb"`/`"appimage"` (Linux) to `formats`, built on a per-OS CI matrix.
- **Release workflow** — a tag-triggered GitHub Actions job (replacing the
  deleted Tauri `release.yml`) that packages + signs + publishes to a draft
  GitHub Release with the updater manifest.
