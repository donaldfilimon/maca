# Contributing to maca

Thank you for your interest in contributing to maca!

## Quick start for contributors

```sh
git clone https://github.com/donaldfilimon/maca.git
cd maca
./check.sh                 # fmt --check, clippy -D warnings, test, cargo deny (if installed), release build, doctor
cargo build --release
eval "$(./target/release/maca shellenv)"
```

## Development workflow

- Run `./check.sh` before every push or PR. It enforces formatting, linting, tests, a release build, and `maca doctor`. It also runs `cargo deny check` when cargo-deny is installed (`cargo install cargo-deny`); CI always runs it.
- CI also builds on Rust 1.89.0, the declared minimum (MSRV), so avoid newer std APIs.
- Run `./scripts/cross-check.sh` (clippy `-D warnings` for Linux and Windows, needs zig) before pushing platform-gated changes.
- Use `RUST_LOG=debug cargo run -- <command>` for tracing during development.
- Prefer small, focused changes.
- Add or update tests when changing behavior (especially anything that touches the ledger, prefix lock, snapshots, or backends).

## Adding or changing backends

maca supports multiple backends (brew, winget, custom, github, aqua). New backends should:

- Expose `search`, `info`, and `plan`/`install` by convention (see existing backends).
- Be callable via `--backend <name>` and participate in `auto` where appropriate.
- Use the shared `install::*` helpers for downloads, verification, extraction, linking, and ledger updates.
- Work with `MACA_*` environment overrides for tests (see `github` and `aqua` for examples).

## Images

All images live in `docs/assets/`. That one location serves both the README and the GitHub Pages site, so an image is never copied into a second place.

- README references are relative to the repository root: `docs/assets/banner.png`. GitHub renders those, and so does any fork. Do not use an absolute `raw.githubusercontent.com` URL in the README — it pins the image to one branch of one fork and breaks everywhere else.
- The Pages site references the same files relative to `docs/`: `assets/banner.png`. Anything that must resolve for a link preview (Open Graph, Twitter cards) needs the absolute Pages URL instead, because those crawlers do not resolve relative paths.
- Prefer PNG. GitHub's image proxy does not reliably render SVG, so an SVG logo will show in the Pages site and vanish in the README.
- Set the display size with an HTML image and a `width`, not Markdown image syntax. Markdown gives GitHub no size hint, so a 1200px banner renders at full resolution and blows out the layout on small screens.
- The social preview GitHub shows on the repository page is not read from any file. It is set by hand under Settings, Social preview, and the image to upload is `docs/assets/social-card.png` (1280×640).

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
