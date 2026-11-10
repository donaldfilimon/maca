# Changelog

All notable changes to maca are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed
- A `maca.toml` that cannot be read or parsed is now an error instead of being
  skipped, so a typo cannot send an install into the global prefix.
- `MACA_CELLAR` now takes precedence over a project or user `cellar` setting,
  as the documentation always said.

### Fixed
- Documentation described version pinning as verify-only and per-directory
  prefixes as planned; both now describe what the code does.

## [0.1.0] - unreleased

First tagged release.

### Added
- Five backends, all over HTTP with no shell-outs: Homebrew (bottles, source
  builds, zip and tar casks), winget (catalog, manifests and dry-run plans
  everywhere, installer execution on Windows), GitHub Releases, the Aqua
  registry, and a custom JSON registry with optional mirrors.
- `--version` and lockfile versions for GitHub Releases and Aqua.
- Exclusive prefix lock, bounded concurrent installs, automatic snapshots and
  rollback on failure, SHA-256 verification, and refusal to overwrite files
  that are not maca's.
- Groups, pins, kinds, lockfiles (`lock`, `sync`, `sync --prune`).
- `shellenv`, `completions`, `doctor`, `outdated`, `clean`, `why`, `leaves`,
  `drift`, `gaps`, `conflicts`, `tui`, and `self-update`.
- Per-directory prefix and cellar through `maca.toml`.
