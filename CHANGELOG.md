# Changelog

All notable changes to maca are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0-alpha.1] - 2026-11-10

First tagged release, published as an alpha pre-release.

### Added
- Five backends, all over HTTP with no shell-outs: Homebrew (bottles, source
  builds, zip and tar casks; disk images and installer packages are refused), winget (catalog, manifests and dry-run plans
  everywhere, installer execution on Windows), GitHub Releases, the Aqua
  registry, and a custom JSON registry with optional mirrors.
- `--version` and lockfile versions on every backend: GitHub Releases and Aqua
  (release tag), Homebrew (versioned formula `name@x`, or the current version
  when it satisfies the request), winget (older manifest directories), custom
  (registration history). A version no catalog can serve is refused, never
  rounded to a nearby one.
- Exclusive prefix lock, bounded concurrent installs, automatic snapshots and
  rollback on failure, SHA-256 verification, and refusal to overwrite files
  that are not maca's.
- Groups, pins, kinds, lockfiles (`lock`, `sync`, `sync --prune`).
- `shellenv`, `completions`, `doctor`, `outdated`, `clean`, `why`, `leaves`,
  `drift`, `gaps`, `conflicts`, `tui`, and `self-update`.
- Per-directory prefix and cellar through `maca.toml`.

### Fixed
- `list`, `info` and `outdated` report a corrupt `pins.json` as an error
  instead of treating it as "no pins".
- `outdated` reports packages whose latest-version lookup failed (stderr
  warning, non-zero exit) instead of printing "all packages are up to date".
- `self-update` says that only stable releases are tracked when it finds no
  release; prereleases are not considered.
- winget ids with more than two segments (`Google.Chrome.Beta`,
  `Python.Python.3.12`) resolve to their nested winget-pkgs directories. A
  two-segment package whose directory also holds numeric sub-package
  directories may still list those as versions.
- A "no releases" or "not found" answer is recognised only from an HTTP 404
  status, not from a repo or package name that contains "404".
- Custom build steps expand `{{prefix}}` in the program word as well as in the
  arguments.
- Group names must be a single path component; `maca group create ../x` and
  similar are refused instead of writing or deleting files outside `groups/`.
- `maca upgrade` no longer reports packages as upgraded after the batch
  rollback reverted them, and says so when the rollback itself fails.
- github installs of archives that keep their executables at the archive root
  (not in `bin/`) now link them; an archive with no executables fails instead
  of reporting a successful empty install.
- github and aqua no longer install `.tar.xz`, `.tar.bz2`, `.tar.zst`, `.gz`,
  `.7z` and similar archives as a "raw" executable; they are skipped (github)
  or refused (aqua).
- `maca upgrade` and `outdated` no longer report a github or aqua package as
  up to date when the release lookup fails (rate limit, offline); the failure
  is reported for that package.
- aqua installs no longer link a dangling binary when `files[].src` names the
  archive's top directory; a file that is not in the archive is an error.
- aqua checksum assets using `{{.Asset}}`, `{{.AssetWithoutExt}}` or
  `{{.SemVer}}` now resolve, so those installs are verified.
- aqua http packages refuse a requested version instead of installing "latest".
- aqua `supported_envs: [all]` now matches every platform.
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
- `rust-toolchain.toml` pins `stable`, and CI builds on
  the declared minimum Rust version (1.89).
- Removed reachable `unwrap`/`expect` panics in the winget version sort and the
  custom backend install path.
- Documentation described version pinning as verify-only and per-directory
  prefixes as planned; both describe what the code does.

### Changed
- A `maca.toml` that cannot be read or parsed is an error, so a typo cannot
  send an install into the global prefix.
- `MACA_CELLAR` takes precedence over a project or user `cellar` setting,
  as the documentation always said.

### Security
- Upgraded `zip` 2.6 to 9.5, which clears four RUSTSEC advisories (symlink and
  permission handling in extraction), `ratatui` 0.29 to 0.30 (drops the
  unsound `lru` 0.12), and replaced the unmaintained `serde_yaml` with
  `serde_norway`. `cargo audit` and `cargo deny check` are clean.
- Zip extraction now refuses absolute and drive-prefixed entry names itself
  (the newer `zip` quietly relativises them) and never applies setuid, setgid
  or sticky bits from an archive.
