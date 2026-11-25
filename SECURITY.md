# Security Policy

## Reporting a vulnerability

Please report security issues **privately**, not via public issues or PRs.

- Email: sebastian.stupak@pm.me
- Or open a GitHub [private vulnerability report](https://github.com/soulaudio/soul-player-ng/security/advisories/new).

Include a description, affected version, and reproduction steps. We aim to
acknowledge within a few days.

## Supply chain

- Rust dependencies are scanned by `cargo audit` (RustSec) and gated by
  `cargo deny` (licenses, bans, sources) in CI; see `.github/workflows/security.yml`.
- A CycloneDX SBOM is generated per CI run.
- The app itself has no JavaScript dependencies: it is all-Rust, with no webview
  and no bundled frontend. The npm dependencies in `website/` build the download
  page and never ship inside the player.

## Update integrity

Application auto-updates are signed with a minisign key and verified against the
public key embedded in the app before installation (see
[app/PACKAGING.md](app/PACKAGING.md)).
