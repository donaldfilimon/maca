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

## Evidence

- `./check.sh` → `check.sh: OK` (38 tests passed, clippy `-D warnings`).
- Live: `maca install jq tree --backend brew` installs concurrently; both
  binaries run.
- Live: `maca install jq --build-from-source` compiles from the formula source
  tarball and `jq --version` prints `jq-1.8.2`.
- Live: custom source package with `--build-step 'make install PREFIX={{prefix}}'`
  builds and runs.
- Live: snapshot → remove → restore returns the package and its binary.
- Live: `maca tui` renders tabs, installed list, search box, and 28 results.
- Live: checksum mismatch rejected; failed install rolled back to its snapshot.

## Not verified (needs hardware the user must provide)

- **Windows installer execution.** No Windows host was available. The code path
  is compile-gated to Windows; only the catalog/manifest/dry-run path ran.
- **Self-update against a real release.** No release exists for the default
  repository; asset selection and checksum logic are unit-tested.
