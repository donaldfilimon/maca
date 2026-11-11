# Changelog

All notable changes to maca are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
- aqua installs no longer link a dangling binary when `files[].src` names the
  archive's top directory; a file that is not in the archive is an error.
- aqua checksum assets using `{{.Asset}}`, `{{.AssetWithoutExt}}` or
  `{{.SemVer}}` now resolve, so those installs are verified.
- aqua http packages refuse a requested version instead of installing "latest".
- aqua `supported_envs: [all]` now matches every platform.

### Changed
- A `maca.toml` that cannot be read or parsed is now an error instead of being
  skipped, so a typo cannot send an install into the global prefix.
- `MACA_CELLAR` now takes precedence over a project or user `cellar` setting,
  as the documentation always said.

### Security
- Upgraded `zip` 2.6 to 9.5, which clears four RUSTSEC advisories (symlink and
  permission handling in extraction), `ratatui` 0.29 to 0.30 (drops the
  unsound `lru` 0.12), and replaced the unmaintained `serde_yaml` with
  `serde_norway`. `cargo audit` and `cargo deny check` are clean.
- Zip extraction now refuses absolute and drive-prefixed entry names itself
  (the newer `zip` quietly relativises them) and never applies setuid, setgid
  or sticky bits from an archive.

### Fixed
- Homebrew bottles tagged `all` (arch-independent) are now selected on arm64
  macOS and Linux instead of falling back to a source build.
- The cask URL allow-list checked a bare prefix, so `http://localhost.evil.com`
  and `http://localhost:80@evil.com` passed; it now shares the stricter
  loopback check used for custom registry mirrors.
- Versioned Homebrew formulae (`name@x`) request the GHCR token for
  `homebrew/core/name/x`, matching where the bottles are published.
- A source build whose formula declares no checksum now warns that it is
  UNVERIFIED.
- The Homebrew index cache is written atomically, a failed cache write no
  longer aborts `search`, and a corrupt cached index is dropped and refetched.
- Casks ignored `--version` and installed the current one; they now refuse a
  version the catalog does not publish.
- `rust-toolchain.toml` pinned `nightly`; it is now `stable`, and CI builds on
  the declared minimum Rust version (1.89).
- Removed reachable `unwrap`/`expect` panics in the winget version sort and the
  custom backend install path.
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
