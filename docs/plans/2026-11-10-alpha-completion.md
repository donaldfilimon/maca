# Alpha completion tranche (2026-11-10)

## Objective

Finish what `v0.1.0-alpha.1` left open without GitHub Actions (the account
had a billing lock, so no job has ever run there): archives for the other
platforms, a real Linux run, the test gaps the audit workflow skipped, and
the small production-plan items that can be verified on this Mac.

## Delivered

- **Four archives on the prerelease.** `aarch64-apple-darwin`,
  `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu` and
  `x86_64-unknown-linux-gnu`, each with a `.sha256`, all built from the tagged
  commit `922af01`.
- **`scripts/package-release.sh <triple>`** cross-builds the other macOS
  architecture with rustup's toolchain; another OS is still refused.
- **`scripts/linux-vm-check.sh [arm64|amd64]`** runs `./check.sh` and the
  packaging on a clean `git archive HEAD` in an Apple `container` Linux VM.
- **Homebrew bottles on Linux** (`src/elf.rs`). Found by installing `jq` in
  the VM: it installed and could not start, because `PT_INTERP` and `RPATH`
  still held `@@HOMEBREW_*@@` tokens. They are rewritten with `patchelf`; a
  host without it gets a refusal and a rollback, and `doctor` warns.
- **Self-update prerelease channel.** `--prerelease`, on by default for a
  prerelease build; `MACA_GITHUB_API` honoured; one backup kept.
- **`scripts/install.sh`**, a checksum-verifying installer.
- **Tests:** self-update end to end on a copy of the binary against a
  loopback server; cask zip with a path escape; installer-package refusal;
  ELF interpreter parsing and token mapping; backup pruning; release ordering.
- A failed unpack no longer leaves an empty `cellar/<name>`.

## Evidence

| Check | Result |
| --- | --- |
| `./check.sh` on macOS arm64 | OK, 147 unit + 29 integration tests |
| `./scripts/cross-check.sh` | OK (Linux and Windows clippy; compile only) |
| `cargo test --target x86_64-apple-darwin` under Rosetta, at `922af01` | 140 + 27 passed |
| `linux-vm-check.sh arm64` at `922af01` (Ubuntu 24.04) | `check.sh: OK`, 140 + 27 passed |
| `linux-vm-check.sh amd64` at `922af01` (emulated) | `check.sh: OK`, 140 + 27 passed |
| Linux arm64 VM, working tree build with `patchelf` | `jq`, `tree`, `fd`, `sqlite` bottles install and run; without `patchelf` the install is refused and rolled back |
| Linux arm64 VM, GitHub backend | `BurntSushi/ripgrep` installs and runs |
| `self-update --force` on a copy, against the real prerelease (macOS arm64) | binary replaced, runs, one backup |
| `scripts/install.sh` against the real prerelease | macOS arm64, Linux arm64, Linux amd64 (piped to `sh`) |
| All eight release assets downloaded back | four checksums OK |

The first Linux run failed one test (`make` missing in the VM image, not a
code defect); the script now installs `make`.

## Still open

Needs GitHub Actions (billing): any CI or Release run; extending the release
matrix (Intel macOS, arm64 Linux, a Windows `.zip`); `cargo deny` on a runner.

Needs a decision or a larger design, not started:

- Casks: `.dmg` via `hdiutil`, `.pkg` payloads, `--link-apps`.
- Taps (third-party Homebrew repositories).
- A hermetic build sandbox (`sandbox-exec` on macOS).
- A per-prefix ledger, so two prefixes can hold different versions.
- Bottle text files that mention the placeholders are not rewritten on
  either OS.

Needs hardware: Windows installer execution and Windows self-update (compile
checked only); self-update against a real release on Linux and on an Intel
Mac (`0.1.0-alpha.1` has no prerelease channel, so that needs a later tag).
