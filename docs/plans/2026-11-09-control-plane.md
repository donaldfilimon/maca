# maca control-plane tranche (2026-11-09)

## Objective

Remove dependence on other package installers, add self-update, package-kind
control, an interactive TUI, source builds, Windows installer execution,
snapshots/rollback, and concurrent installs.

## Delivered

- **Independence**: no `brew`/`winget` binary is invoked for catalog or brew
  installs. Remaining external tools are Apple developer tools
  (`install_name_tool`, `codesign`) for Mach-O relocation and `msiexec` for MSI
  on Windows. HTTP has a 20 s timeout so a hung catalog cannot stall a command.
- **Concurrent installs**: exclusive prefix lock (`flock`/`LockFileEx`), a
  bounded job pool (`--jobs`), concurrent backend searches, and serialized
  ledger writes.
- **Snapshots & rollback**: automatic snapshot before `install`/`remove`/
  `upgrade`; automatic rollback on failure; manual
  `snapshot create|list|restore|delete`.
- **Source builds**: brew `--build-from-source` (Autotools/CMake/Meson/Make/
  Cargo/Python detection) and custom-registry `build_steps` with `{{prefix}}`
  substitution and shell-style quoting.
- **Control plane**: package kinds (`app`/`plugin`/`cli`/`library`/`other`),
  groups/profiles, lockfiles (`lock`/`sync`), pins, `why` reverse lookups.
- **TUI**: `maca tui` with Installed/Search/Groups/Snapshots tabs.
- **Self-update**: GitHub release fetch, target asset selection, `.sha256`
  verification, backup-and-replace.
- **Catalog cache**: the 31 MiB brew index is cached for 1 hour.

## Evidence (current as of 2026-11-10, after completion tranches)

- `./check.sh` → `check.sh: OK` (121 tests passed: 106 unit + 15 integration, clippy `-D warnings`, release build, doctor self-check).
- `./scripts/cross-check.sh` → OK for `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc` (clippy `-D warnings` via zig cross-compiler; proves Windows code paths compile and lint without a Windows machine).
- Live: `maca install jq tree --backend brew` (concurrent), `--build-from-source`, custom `build_steps`, snapshots/restore, pin/upgrade/lock/sync, TUI, packaging (`scripts/package-release.sh`), and `maca self-update --check` all functional.
- 14 dedicated selfupdate unit tests cover asset selection, SHA-256 verification, hardened extraction (zip-slip, size bounds, unsafe paths), backup/restore on failure, and target matching. `maca self-update --check` correctly reports "no releases published" when none exist.
- All Winget catalog + manifest resolution works; `install` path is gated behind `#[cfg(target_os = "windows")]` with explicit error on non-Windows.

## Blocked (requires hardware or external state the user must provide)

- **Windows installer execution.**
  Code exists and is cross-lint verified. Real execution (msiexec for .msi, /S for EXE) only happens under `#[cfg(target_os = "windows")]`. On non-Windows it correctly refuses with a clear message.
  To verify: run on a real Windows machine: `maca install <valid-winget-id> --backend winget` (or the packaged binary after self-update).

- **Self-update against a real published release.**
  Logic, checksum, extraction, and swap are complete and unit-tested. No `v*` release tag + matching assets have been published to the default repo yet (`donaldfilimon/maca`).
  Once published: `maca self-update` (or `--check`) will fetch, verify the `.sha256`, extract the exact host triple asset, back up the old binary, and replace it.

No other items from the objective remain unimplemented on this platform.
