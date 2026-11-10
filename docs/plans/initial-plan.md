# maca initial plan (2026-11-09)

## Goal

A cross-platform (macOS + Linux) package manager in Rust 2024 that installs
packages from Homebrew bottles, the winget catalog, and a custom registry —
without requiring the native package manager binaries for catalog operations.

## Decisions

- **Location:** `~/maca` (standalone on the local drive alongside `~/sources/repos`).
- **Backends:** native HTTP APIs, not subprocesses.
  - brew: `formulae.brew.sh` formula JSON + GHCR OCI bottle blobs.
  - winget: `api.winget.run` search + `microsoft/winget-pkgs` manifests.
  - custom: local JSON registry plus optional HTTP mirrors.
- **Install prefix:** `<data>/prefix` with `cellar/`, `opt/`, and `bin/`,
  matching Homebrew's layout so bottle paths resolve.
- **Verification:** every artifact is SHA-256 checked against its catalog
  metadata before extraction.
- **Ledger:** `installed.json` records name, version, backend, cellar path.

## Delivered

- `search`, `info`, `install`, `list`, `upgrade`, `remove`, `doctor`.
- `custom add/list/remove` registry management.
- Recursive Homebrew dependency install.
- Mach-O placeholder rewrite (`@@HOMEBREW_CELLAR@@`, `@@HOMEBREW_PREFIX@@`)
  plus ad-hoc re-sign so bottle binaries run on Apple Silicon.
- 23 unit tests; `./check.sh` runs fmt, clippy `-D warnings`, test, release
  build, and a self-check.

## Evidence

- `./check.sh` → `check.sh: OK` (23 tests passed).
- Live: `maca install jq --backend brew` → `jq-1.8.2` runs.
- Live: `maca install ripgrep --backend brew` → `ripgrep 15.2.0` runs
  (pulls the `pcre2` dependency).
- Live: custom tarball install → binary runs.
- Live: winget `--dry-run` resolves `Microsoft.VisualStudioCode 1.143.1` with
  the correct arm64 installer URL.

## Completed (subsequent tranches)

All items below were delivered in later work:

- Source builds (`--build-from-source` + custom `build_steps`).
- Snapshots + rollback before/after mutating commands.
- Concurrent installs with prefix lock + `--jobs`.
- Packaging + CI automation (`scripts/package-release.sh`, `.github/workflows/{ci,release}.yml`).

## Blocked (requires hardware / external state the user must provide)

- **Windows installer execution**: The code path exists and is compile/lint-verified via `scripts/cross-check.sh` for `x86_64-pc-windows-msvc`. Execution (msiexec / EXE) only runs under `#[cfg(target_os = "windows")]`. Verified command on a Windows host: `maca install <winget-pkg> --backend winget`.
- No real GitHub release has been published yet for `donaldfilimon/maca`, so end-to-end `maca self-update` cannot be exercised against a live release. Logic, asset selection, checksum verification, extraction hardening, and backup/restore are fully unit-tested (14 selfupdate tests).
