# Contributing

Thanks for taking an interest. Bug reports, fixes, and ideas are all welcome.
Taking part means following the [Code of Conduct](CODE_OF_CONDUCT.md), which is
short and amounts to "be decent to people".

## Before you start

For anything larger than a bug fix, open an issue first and say what you have in
mind. It is better to hear "we already tried that, here is why it did not work"
before you write the code than after.

## Getting set up

[docs/DEVELOP.md](docs/DEVELOP.md) covers the toolchains (stable for the backend,
host, and server; a pinned nightly for the Rune UI head), the Linux system
libraries, the Nix shells, and how to run each test suite.

## The rules the codebase keeps

These are enforced by CI, so it is cheaper to know them up front:

- **No `unsafe`.** The whole workspace is `forbid(unsafe_code)`.
- **Clippy is clean.** `cargo clippy --all-targets -- -D warnings` must pass.
- **`cargo fmt` is law.** No hand formatting.
- **The UI is just visualization.** Logic lives in the backend or the host, never
  in a view. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); a few of these
  boundaries are enforced by architecture tests and `cargo deny`.
- **New behaviour comes with a test.** Audio bugs are hard to spot by eye and
  harder to spot by ear.

## Pull requests

- Branch off `main`.
- PR titles follow [Conventional Commits](https://www.conventionalcommits.org)
  (`fix(output): ...`, `feat(library): ...`). CI checks this.
- Keep a PR to one change. Two unrelated fixes are two PRs.
- Say what you did and why. If it is a bug fix, say how to reproduce the bug.
- Run `cargo test` and `cargo clippy` locally first. CI runs on Linux, macOS, and
  Windows, so a green local run is a good sign but not a guarantee.

## Reporting bugs

Use the issue template. The useful details are: your OS and version, the app
version, the audio device and output mode, the file format involved, and what you
expected to happen instead. A log file beats a description.

For security issues, do not open a public issue: see [SECURITY.md](SECURITY.md).

## Licensing

The project is Apache 2.0. By contributing you agree your contribution ships under
that license, per section 5 of the license text.
