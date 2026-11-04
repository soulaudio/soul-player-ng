# Release, auto-update, and CI setup

This repo ships a signed, self-updating Tauri desktop app. The pipeline mirrors
`soulaudio/soul-player`, adapted to this single-app layout.

## One-time repository secrets

| Secret | Required for | How to get it |
| --- | --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | Signing update bundles so the app will accept them | The minisign private key generated below. |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Only if the key has a password | The password you set (ours has none → set empty or omit). |
| `CORE_REPO_TOKEN` | CI/release fetching the **private** `soul-audio-core` git dependency | A fine-grained PAT with read access to `soulaudio/soul-audio-core`. **Not needed once that repo is public** — the auth step auto-skips when the secret is empty. |

Set them with the CLI, e.g.:

```sh
gh secret set TAURI_SIGNING_PRIVATE_KEY   < .tauri/soul-player-updater.key
gh secret set TAURI_SIGNING_PRIVATE_KEY_PASSWORD --body ""
gh secret set CORE_REPO_TOKEN             --body "<pat>"
```

## Updater signing key

Generated once with `npx @tauri-apps/cli signer generate`:

- **Private key**: `.tauri/soul-player-updater.key` — gitignored, **never commit it**. Register it as `TAURI_SIGNING_PRIVATE_KEY`.
- **Public key**: `.tauri/soul-player-updater.key.pub`, already pasted into `app/src-tauri/tauri.conf.json` under `plugins.updater.pubkey`. The app verifies every update against it — a mismatched key makes all auto-updates fail.

To rotate: regenerate, replace the `pubkey` in `tauri.conf.json`, and update the secret.

## Cutting a release

The app version lives in `app/src-tauri/tauri.conf.json` (`version`) and `app/package.json`. Keep them in sync.

1. Bump the version in both files.
2. Merge to `main`. **auto-release-on-version-bump.yml** sees the changed `tauri.conf.json`, creates `v<version>`, and pushes it.
3. **release.yml** (tag-triggered) builds and signs bundles for macOS (arm64 + x64), Linux, and Windows via `tauri-apps/tauri-action`, uploads them to a **draft** GitHub Release, and publishes `latest.json` (the updater manifest).
4. Review the draft release and publish it. The app's updater polls
   `https://github.com/soulaudio/soul-player-ng/releases/latest/download/latest.json`.

You can also push a `v*` tag by hand to trigger step 3 directly.

## Auto-update behaviour

- On launch (unless `SOUL_PLAYER_SKIP_UPDATE` is set) the app checks the release
  `latest.json`, verifies the signature, and emits an `update-available` event
  (`{ version, notes, date }`) for the frontend to prompt on. Install + relaunch
  is handled via `@tauri-apps/plugin-updater` / `tauri-plugin-process`.
- Signing is **ad-hoc on macOS** and **unsigned on Windows** (no Apple/Authenticode
  certs wired), so users see Gatekeeper/SmartScreen warnings on first install.
  Add `APPLE_*` / Authenticode secrets + config to remove them.
- Linux auto-update requires the AppImage target (deb/rpm are install-only).

## CI

- **ci.yml** — `rust` (fmt · clippy · test for the service and app workspaces) and
  `frontend` (svelte-check · vitest · vite build), gated by `ci-success`.
- **security.yml** — `cargo audit`, `cargo deny` (licenses/bans/sources), PR
  dependency review, CycloneDX SBOM, and `npm audit`.
- **pr-title.yml** — Conventional Commits + no tool-attribution (the squash
  subject policy, matching the local `commit-msg` hook).

Local git hooks (`lefthook install`) run fmt/clippy on commit and tests +
frontend checks on push.
