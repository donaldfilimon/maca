# maca completion tranche (2026-11-09)

## Objective

Elevate `maca` to a complete, autonomous, production-ready package manager: environment integration (`shellenv`), shell completions (`completions`), man page/share asset linking, fallback binary discovery, health diagnostics in `maca doctor`, outdated version auditing, cache and snapshot pruning (`maca clean`), declarative lockfile pruning (`sync --prune`), release packaging automation (`scripts/package-release.sh`), and cross-platform GitHub Actions CI.

## Delivered

- **Environment & Shell Integration**: `maca shellenv` exports `MACA_PREFIX`, `PATH`, `MANPATH`, and `INFOPATH` for `eval "$(maca shellenv)"` across `bash`, `zsh`, `fish`, and `csh`.
- **Shell Completions**: `maca completions <shell>` powers autocompletion for `bash`, `zsh`, `fish`, `elvish`, and `powershell` via `clap_complete`.
- **Share & Documentation Assets**: `install::link_share` and `unlink_share` recursively symlink man pages (`share/man`), info documentation (`share/info`), and shell completions into `<prefix>/share/...` during installs and cleanly unlink them upon removal.
- **Binary Discovery Fallback**: `install::link_bins` automatically scans `<cellar>/bin` and `<cellar>/sbin` when metadata lacks explicit binary listings, ensuring custom and source builds always expose their executables.
- **Enhanced Diagnostics in `maca doctor`**: Probes prefix accessibility and writability, validates `$PATH` configuration, inspects required developer tools (`install_name_tool`, `codesign`, `otool`, compilers), tests catalog API connectivity (`formulae.brew.sh`, `winget.run`), checks for broken symlinks, and exits with structured status codes.
- **Outdated Package Inspection**: `maca outdated` non-mutatively audits installed packages against current catalog versions, displaying pin states and supporting machine-readable `--json`.
- **Cache & Disk Hygiene**: `maca clean` reclaims disk space across index caches, build logs, and temporary build trees; `maca snapshot prune --keep N` prunes older snapshots.
- **Declarative Sync Pruning**: `maca sync --prune` enforces exact alignment with `maca.lock.json` by unlinking and removing unpinned packages.
- **Packaging & CI**: `scripts/package-release.sh` builds and packages release archives matching `selfupdate` conventions (`maca-<target>.tar.gz` and companion `.sha256`); `.github/workflows/ci.yml` runs full verification across macOS and Linux.
- **Integration Test Suite**: Added `tests/integration_tests.rs` with 7 end-to-end test cases covering CLI flags, environment output, completions, diagnostics, and symlink lifecycles.

## Evidence

- `./check.sh` → `check.sh: OK` (121 tests passed: 106 unit + 15 integration tests, clippy `-D warnings`, release build).
- Live: `maca shellenv` outputs valid export commands for sh/bash, fish, and csh.
- Live: `maca completions` emits functional scripts for bash, zsh, fish, elvish, powershell.
- Live: `maca doctor` diagnoses prefix, PATH, toolchain, catalog endpoints, symlinks with structured summary.
- Live: `maca snapshot prune --keep 2`, `maca clean`, `maca outdated --json`, `maca sync --prune` all functional.
- Live: `maca list` shows PINNED column; `maca pin`/`unpin` and `maca info` reflect pinned state.
- `scripts/package-release.sh` produced `maca-<triple>.tar.gz` + `.sha256` matching self-update expectations.
- `.github/workflows/ci.yml` runs check.sh + packaging on macOS/Linux and clippy+test on Windows.
- `.github/workflows/release.yml` packages and uploads on `v*` tags (tag/version gate included).
- Integration tests cover shellenv, completions, doctor, binary discovery, share linking, pin/upgrade/lock/sync/restore, snapshot isolation, clean, and more.
