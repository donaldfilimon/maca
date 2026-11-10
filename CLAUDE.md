# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`maca` is a Rust 2024 (MSRV 1.85) package manager for macOS and Linux. It installs from three backends — Homebrew bottles/sources, the winget catalog, and a custom JSON registry — by calling their HTTP APIs directly. It never shells out to `brew` or `winget`. One crate, one binary (`src/main.rs`) on top of a library (`src/lib.rs`).

## Commands

```sh
./check.sh                                           # the gate: fmt --check, clippy, test, release build, `maca doctor`
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib                                     # unit tests only
cargo test --lib backend::tests::backend_kind_roundtrip   # one unit test: <module>::tests::<name>
cargo test --test integration_tests test_binary_discovery # one integration test
RUST_LOG=debug cargo run -- search jq                # tracing goes through RUST_LOG
./scripts/package-release.sh [target-triple]         # writes target/dist/maca-<triple>.tar.gz + .sha256 (host triple only)
./scripts/cross-check.sh                             # clippy -D warnings for Linux + Windows from macOS (needs zig; compile-only)
```

CI (`.github/workflows/ci.yml`) runs `./check.sh` then `./scripts/package-release.sh` on Ubuntu and macOS, plus clippy and `cargo test` on Windows. `.github/workflows/release.yml` runs on a `v*` tag, checks the tag equals the `Cargo.toml` version, and uploads the archives to the GitHub release. Neither workflow has run yet (the repo has never been pushed with them).

### Test isolation

`tests/integration_tests.rs` spawns the built binary (`CARGO_BIN_EXE_maca`) with `MACA_HOME` pointed at a temp dir and `MACA_PREFIX` cleared: `maca_cmd` for the simple command tests, and a per-test `Sandbox` for the `e2e_*` tests. Never use a bare `Command::new` on the binary, or the test acts on the developer's real `~/.maca`.

The `e2e_*` tests are the pattern for anything that installs: `serve()` runs a loopback HTTP server holding tarballs built in the test, packages are registered with `custom add`, and the test asserts on the ledger, the cellar, the bin links and the output of the installed script. They need no network.

Two things still reach outside the sandbox: the `doctor` test probes the catalog endpoints over the network, and `check.sh` ends by running `maca doctor` against the real prefix, failing if doctor reports any failure on the host (missing tools, unreachable catalog, broken symlinks).

## Architecture

### Two state roots

State is split, and only one half is relocatable:

- **Prefix** — `--prefix` > `MACA_PREFIX` > `prefix` in `config.toml` > `<data>/prefix`. Holds `cellar/<name>/<version>/`, `opt/<name>`, `bin/`, `share/`, the process lock `.maca.lock`, and the lockfile `maca.lock.json`.
- **Data dir** — `config::data_dir()`: `MACA_HOME` if set, else `~/.maca` on macOS and the XDG data dir + `/maca` elsewhere. Holds `config.toml`, the ledger `installed.json`, `pins.json`, `custom-registry.json`, `groups/`, `snapshots/`, `cache/brew-index.json`, `logs/`.

Consequence: pointing `MACA_PREFIX` (or `--prefix`) at a temp dir does **not** isolate the ledger, pins, snapshots or registry — `state::Ledger::load()` and `snapshot::*` always read the data dir, and ledger records store absolute cellar paths. Use `MACA_HOME` to isolate everything; there is no CLI flag for it.

The default data dir is deliberately space-free on macOS (not `~/Library/Application Support`): source build recipes interpolate the prefix unquoted. Keep any new default path space-free.

### Backends are structs, not a trait

`BrewBackend`, `WingetBackend` and `CustomBackend` each expose `search` / `info` / `plan` / `install` by convention; there is no shared trait. Dispatch is a `match` on `BackendKind` or on the ledger's backend string, repeated in several places: `backend::install_with_backend`, and in `main.rs` `cmd_search`, `info_one`, `install_many`, `latest_version`. A new backend or a signature change has to be applied at each site.

`--backend auto` means custom → brew → winget: `search_all` queries all three concurrently and merges; `auto_install` takes the first backend whose `plan()` succeeds.

Custom registry HTTP mirrors (`custom_registry_urls`) are additive and local entries win. Install and auto-search paths load them through `Registry::load_configured()`, which falls back to the local registry if a mirror is unreachable; `custom add/list/remove` and `doctor` use `load_local()` only.

### Install pipeline (`install.rs` shared helpers)

Brew bottle install, in order: fetch formula JSON → recurse into `dependencies` (skipping `uses_from_macos`) → anonymous GHCR bearer token → download → SHA-256 verify → extract to cellar → `link_opt` → `mach_o::rewrite_placeholders` → `link_bins` → `link_share` → ledger upsert.

- `mach_o.rs` substitutes `@@HOMEBREW_CELLAR@@` / `@@HOMEBREW_PREFIX@@` with `install_name_tool`, then every modified Mach-O must be ad-hoc re-signed (`codesign --force --sign -`) or Apple Silicon kills it at launch. It is a no-op off macOS.
- Extraction is staged: `extract_bottle` / `extract_tar_gz_to_cellar` unpack into a unique `cellar/.tmp-*` dir and `rename` into place, so a keg that exists is complete (both return early when the keg exists). Package names and versions are checked with `install::valid_component` before they become path components.
- Links go through `install::place_link`, which replaces an existing entry only if it is a dangling symlink or points into the same package's `cellar/<name>`; anything else is left alone and reported. `link_bins` falls back to scanning `<cellar>/bin` and `sbin` when the metadata lists no executables.
- State files (ledger, pins, lockfile, snapshot manifest) are written with `install::atomic_write`.
- Lockfile and group versions are verify-only: backends always install the catalog's current version, and `check_locked_version` in `main.rs` turns a mismatch into a failure.
- `source.rs` handles `--build-from-source` (build-system detection) and custom-registry `build_steps` (`{{prefix}}` substitution, shell-style word splitting); logs go to `<data>/logs/`.
- winget installer execution is `cfg(windows)`-gated; on macOS/Linux only catalog, manifest resolution and `--dry-run` plans run.
- Artifact downloads (`install::download`) have connect and idle-read timeouts but no total limit; `http_client` (small API calls) has a 20 s total timeout.

### Locking, snapshots and concurrency live in `main.rs`

Library functions do not take locks or snapshots; the `cmd_*` handlers do:

- `lock::PrefixLock::acquire(&ctx.prefix)` (exclusive `flock` on `<prefix>/.maca.lock`) is taken at the top of each mutating handler and for the whole TUI session. It is not re-entrant — a second acquire in the same process blocks — so never acquire it in code a handler calls.
- Mutating handlers (`install`, `remove`, `upgrade`, `sync`, `group install`, and each TUI action in `tui.rs`) take one snapshot per command, then `snapshot::restore` it if the operation fails. `install_many` forces `no_snapshot: true` on each per-package task for that reason. `restore` is a full rollback: it deletes kegs that did not exist at snapshot time (the manifest lists every keg directory present then, so kegs maca never knew about survive), re-clones the snapshotted kegs, sweeps dangling maca links, and writes the ledger last. Handlers must return `Err` for a failed operation or the rollback never fires — `upgrade`, `sync` and `group install` all exit non-zero on a failed package.
- Multi-package work fans out with `tokio::spawn` bounded by a `Semaphore` of `--jobs`. Ledger writes must go through `state::upsert_record` / `remove_record` (in-process mutex plus a file lock on `<data>/.ledger.lock`, since the ledger is shared across prefixes), not a bare `Ledger::load()`…`save()`.

Naming trap: `lock.rs` is the cross-process prefix mutex; the reproducibility **lockfile** (`maca lock` / `maca sync`) and groups live in `group.rs`. Pins are a third thing (`state::load_pins` / `save_pins`, `pins.json`): a pinned package is held at its installed version, and a corrupt pins file is an error, never "no pins".

### Self-update

`selfupdate::pick_asset` matches release assets named exactly `maca-<target-triple>.tar.gz` plus `…tar.gz.sha256`, which is what `scripts/package-release.sh` produces. The asset name carries no version; the release tag does.

Platform-gated code must stay warning-free on every target: macOS-only helpers that unit tests still use get `#[cfg_attr(not(target_os = "macos"), allow(dead_code))]`, and Unix-only test helpers get `#[cfg(unix)]`. Run `./scripts/cross-check.sh` before pushing; `check.sh` on a Mac cannot see these.

## Scope notes

- Windows installer execution and self-update against a real release are implemented but have never been run (no Windows host; no published release). Don't describe them as verified.
- Plans go in `docs/plans/`; each tranche plan records what was delivered and the evidence for it.
