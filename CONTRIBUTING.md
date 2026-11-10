# Contributing to maca

Thank you for your interest in contributing to maca!

## Quick start for contributors

```sh
git clone https://github.com/donaldfilimon/maca.git
cd maca
./check.sh                 # fmt --check, clippy -D warnings, test, release build, doctor
cargo build --release
eval "$(./target/release/maca shellenv)"
```

## Development workflow

- Run `./check.sh` before every push or PR. It enforces formatting, linting, tests, a release build, and `maca doctor`.
- Use `RUST_LOG=debug cargo run -- <command>` for tracing during development.
- Prefer small, focused changes.
- Add or update tests when changing behavior (especially anything that touches the ledger, prefix lock, snapshots, or backends).

## Adding or changing backends

maca supports multiple backends (brew, winget, custom, github, aqua). New backends should:

- Expose `search`, `info`, and `plan`/`install` by convention (see existing backends).
- Be callable via `--backend <name>` and participate in `auto` where appropriate.
- Use the shared `install::*` helpers for downloads, verification, extraction, linking, and ledger updates.
- Work with `MACA_*` environment overrides for tests (see `github` and `aqua` for examples).

## Commit style

Use clear, descriptive commit messages. When the change affects user-facing behavior, include a short example in the commit message if helpful.

## Pull requests

- Keep PRs focused. One feature or fix per PR is ideal.
- Update relevant documentation (README, help text, or man-page style descriptions in code).
- Ensure `./check.sh` is green locally before opening or pushing to a PR.
- CI will run on push and on PRs (including cross-targeted clippy for Linux + Windows via zig).

## Code of conduct

This project is licensed under the Apache License 2.0 (see [LICENSE](LICENSE)).

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating you are expected to uphold this code.

## Reporting security issues

See [SECURITY.md](SECURITY.md) for how to report vulnerabilities.

Thank you for helping make package management on macOS and Linux better!
