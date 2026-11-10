# maca

[![CI](https://github.com/donaldfilimon/maca/workflows/CI/badge.svg)](https://github.com/donaldfilimon/maca/actions)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![Rust](https://img.shields.io/badge/rust-1.89%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-3fb950?logo=github)](https://donaldfilimon.github.io/maca/)

Cross-platform package manager for **macOS** and **Linux**, written in **Rust 2024**.

<p align="center">
  <img src="docs/assets/banner.png" alt="maca — one binary, five ecosystems: Homebrew, winget, GitHub Releases, Aqua, and custom, with no shellouts" width="100%">
</p>

> **Live documentation & demo site:** [https://donaldfilimon.github.io/maca/](https://donaldfilimon.github.io/maca/)

maca installs packages from five sources **without shelling out to the native
package manager** (or to `gh` or `aqua`) for catalog or install work:

| Backend  | Catalog                                                                 | Installs                                                                                                   |
| -------- | ----------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| `brew`   | Homebrew formula JSON API (`formulae.brew.sh`)                          | Bottles (OCI blobs from GHCR) **or source builds**                                                         |
| `winget` | `api.winget.run` search + `microsoft/winget-pkgs` manifests             | Windows installers (MSI/EXE) on Windows; dry-run plans everywhere                                          |
| `custom` | Local JSON registry + optional HTTP mirrors                             | `tar.gz`, single binaries, or source trees with build steps                                                |
| `github` | GitHub Releases API (`owner/repo` or `owner--repo`)                     | Host-matching `.tar.gz`, `.zip`, or a raw binary. `MACA_GITHUB_API` overrides the API origin               |
| `aqua`   | aqua standard registry (`name/name`, `name--name`, or `aqua:name/name`) | `github_release` assets and concrete `http` URLs. `MACA_AQUA_INDEX` and `MACA_AQUA_RAW` override the index |

`--backend auto` (the default) searches custom → brew → winget → github → aqua.
Install uses the first of custom, brew, then a `owner/repo` or `aqua:` spec,
then winget. Select a source by name with `--backend github` or `--backend aqua`.

## Why maca

One machine. Several catalogs. One binary. maca installs from Homebrew, winget,
GitHub Releases, the Aqua registry, and a registry of your own without making
you install a package manager for each of them first.

|                                             | maca                                           | Homebrew                            | winget         | aqua                    | Nix                                     |
| ------------------------------------------- | ---------------------------------------------- | ----------------------------------- | -------------- | ----------------------- | --------------------------------------- |
| Runs on                                     | macOS and Linux, Windows in progress           | macOS (Linux is a separate install) | Windows        | macOS, Linux, Windows   | macOS, Linux                            |
| Installs without the other tool present     | yes                                            | it is the tool                      | it is the tool | it is the tool          | it is the tool                          |
| Catalogs from one binary                    | five, and growing                              | one                                 | one            | one                     | one, from source descriptions           |
| A failed install puts the machine back      | automatic snapshot and rollback                | no                                  | no             | no                      | the store waits for a generation switch |
| Two installs at once stay consistent        | exclusive prefix lock                          | no                                  | no             | no                      | builds are isolated                     |
| An environment you can hand to someone else | fetched for GitHub and Aqua, checked otherwise | `Brewfile`                          | no             | version-pinned registry | fully hermetic                          |

**Five catalogs, zero extra tools.** Homebrew formula JSON, the winget catalog,
GitHub Releases, the Aqua registry, and a local registry are HTTP APIs maca
calls itself. Nothing has to be installed first — not `brew`, not `winget`, not
`gh`, not `aqua`. Each of those tools does one catalog. maca does all five, and
a sixth is a backend, not a new product.

**Installs that fail undo themselves.** Every mutating command snapshots the
prefix before it touches anything. If it fails, the kegs, links, and ledger
entries it created come back out and the previous ones go back in. Homebrew
keeps old kegs but leaves a failed multi-package install where it stopped.
winget and aqua install forward and stop. Two maca processes share one exclusive
prefix lock, so they cannot race the cellar or the ledger.

**Verified before it lands, and nothing of yours gets overwritten.** Every
artifact that declares a SHA-256 is checked before it is unpacked. On macOS and
Linux a binary or man page that already belongs to you, or to another package,
is left alone and reported instead of replaced.

**An environment you can commit.** Groups name a set of packages. Pins hold one
at the version you installed. `maca.lock.json` records the whole prefix, and
`maca sync` installs exactly what it names — and rolls back if the version it
gets is not the one the lock recorded. That is a declared environment today,
and fetching a specific older artifact (the way Nix and aqua already can) is the
piece being added on top of it, not a different design.

### On the way

The gaps against the specialists are features in progress, not reasons to pick
something else.

- **Pinned versions, fetched where the catalog can serve them.** GitHub
  Releases and the Aqua registry already do: `--version`, a group version, and
  a lockfile version fetch that release instead of the current one (the tag is
  tried with and without its leading `v`). Homebrew, winget, and a custom entry
  only publish one version, so they refuse the request rather than installing
  the wrong one. `maca sync` still rolls back if what comes back is not what the
  lock named.
- **Per-directory versions.** asdf and mise switch language versions per
  project. maca installs into a prefix today; a directory-scoped prefix on top
  of the same cellar is the planned shape, so version switching reuses the
  installs you already have.
- **The rest of Homebrew.** Bottles and source builds from formula JSON work
  now. Taps, casks, and services are the remaining Homebrew surface, and each
  is another catalog call rather than a dependency on Homebrew's Ruby DSL.
- **Hermetic builds.** Source builds already detect Autotools, CMake, Meson,
  Make, Cargo, and Python. Sandboxed, reproducible builds in the Nix sense are
  the longer piece of that same pipeline.
- **Windows installs, not just Windows plans.** The winget catalog, manifest
  resolution, checksum verification, and dry-run plans run today. The installer
  launch path is written and compile-gated to Windows, and is waiting on a
  Windows host to be exercised — see [Limits](#limits-honest-scope) for exactly
  what has and has not run.

## Install

```sh
cargo build --release
eval "$(./target/release/maca shellenv)"
```

Override the prefix with `--prefix` or `MACA_PREFIX`. Set `MACA_HOME` to move
the whole data directory (ledger, pins, snapshots, groups, registry, cache,
logs and the default prefix) somewhere other than `~/.maca`.

## Commands

```sh
maca shellenv                      # export PATH, MACA_PREFIX, MANPATH, INFOPATH
maca shellenv --shell fish         # fish shell configuration
maca completions zsh > ~/.zfunc/_maca # shell completions (bash, zsh, fish)
maca search jq                     # search all backends (cached brew index)
maca search jq --json              # machine-readable
maca info jq
maca install jq                    # installs dependencies too
maca install jq tree --jobs 4      # concurrent installs
maca install owner/repo --backend github --version 14.1.0
maca install jq --build-from-source
maca install code --kind app       # kind: app|plugin|cli|library|other
maca app install code              # kind-specific aliases
maca plugin install ...
maca cli install jq
maca list --kind cli
maca outdated                      # check for newer versions without upgrading
maca outdated --json
maca upgrade                       # honours pins
maca remove jq
maca why oniguruma                 # reverse dependencies
maca leaves                        # installed packages nothing else depends on
maca drift                         # pins that differ from the installed version
maca gaps dev                      # group members missing or at the wrong version
maca conflicts                     # two packages claiming the same binary

maca group create dev
maca group add dev jq --backend brew --kind cli
maca group install dev             # concurrent
maca group export dev              # writes a lockfile
maca lock                          # lockfile from the ledger
maca sync                          # install what the lock names; fails if a version differs
maca sync --prune                  # also remove packages the lock does not name

maca pin jq                        # pin to the installed version
maca unpin jq

maca snapshot create --note "before upgrade"
maca snapshot list
maca snapshot diff <older> <newer> # added, removed, and version changes
maca snapshot restore <id>
maca snapshot delete <id>
maca snapshot prune --keep 5       # keep N latest snapshots
maca clean                         # clean logs, tempdirs, prune snapshots
maca clean --all                   # also clean 31 MiB catalog cache

maca self-update --check
maca self-update
maca tui                           # interactive terminal UI
maca doctor                        # diagnostics: prefix, toolchain, network, symlinks
maca custom add --name hello --version 1.0.0 \
  --url https://example.com/hello.tar.gz --sha256 <hex>
maca custom list
maca custom remove hello
```

## Safety

- **Exclusive prefix lock** (`<prefix>/.maca.lock`, `flock`/`LockFileEx`) so
  concurrent maca processes cannot race the ledger or cellar. `upgrade` (CLI
  and TUI) checks catalog versions **without** the lock, then takes it only to
  change state and re-validates each planned upgrade against the ledger and
  pins at that point, so a plan that went stale (another upgrade, a new pin,
  a removal) is skipped rather than applied. This holds by construction; the
  CLI orchestration is unit-tested with fakes (lock free during checks, held
  during apply, stale plans skipped), the TUI path only by a source-ordering
  test, and no test runs two real processes. Packages recorded under another
  prefix are reported as skipped, not upgraded.
- **Concurrent installs** with a bounded job pool (`--jobs`, default 4); ledger
  writes are serialized.
- **Automatic snapshot** before `install`/`remove`/`upgrade`/`sync`/`group
install` and before each TUI action (disable
  with `--no-snapshot` where offered). A failed operation rolls the prefix back
  automatically: kegs, links and ledger entries created after the snapshot are
  removed, and the snapshotted ones are put back.
- **SHA-256 verification** of every artifact that declares a checksum. Bottles
  always do. A custom registry entry without `sha256` is installed with a
  loud warning; the hash guards against a corrupted or swapped download, not
  against a registry you should not trust.
- **Nothing foreign is overwritten** (macOS and Linux): a `bin/` or `share/`
  entry that belongs to another package or is a file of your own is left in
  place and reported. On Windows these entries are copies, which carry no
  owner, so an existing one is replaced; uninstall removes only copies that
  are still identical to the package's files.
- **Staged extraction and atomic state writes**: a keg appears at its final
  path only after a complete unpack, and the ledger, pins, lockfile and
  snapshot manifests are written through a temp file and a rename.
- **Custom registry mirrors** must be `https://` (plain `http://` is accepted
  only for localhost). Package names and versions that are not plain path
  components are refused.
- **Mach-O relocation**: bottles are rewritten with `install_name_tool`
  (`@@HOMEBREW_CELLAR@@`, `@@HOMEBREW_PREFIX@@`) and ad-hoc re-signed with
  `codesign`, so they run on Apple Silicon. These are Apple developer tools,
  not package managers; no `brew` binary is required.
- **Space-free default prefix** (`~/.maca/prefix`) so source build recipes that
  split on whitespace work.

## Source builds

`maca install <formula> --build-from-source` downloads the formula's source
tarball, verifies its checksum, installs dependencies, detects the build system
(`configure`, CMake, Meson, Make, Cargo, Python), compiles, and installs into
the cellar. Build logs land in `<data>/logs/`.

The custom backend supports build steps directly:

```sh
maca custom add --name hello --version 1.0.0 \
  --url https://example.com/hello-src.tar.gz --sha256 <hex> \
  --build-step 'make install PREFIX={{prefix}}'
```

Steps are split with shell-style quoting, so paths with spaces work; `{{prefix}}`
expands to the install destination.

## Groups, kinds, and lockfiles

- **Kinds** — `app`, `plugin`, `cli`, `library`, `other` — recorded per package
  and filterable (`maca list --kind cli`).
- **Groups** — named profiles of packages with optional versions, installed as
  a unit. `group install` exits non-zero if any member fails or resolves to a
  different version than the group names.
- **Lockfiles** — `<prefix>/maca.lock.json` records name, version, backend and
  kind. The catalogs only serve their current version, so `maca sync` cannot
  fetch an older one: it installs each locked package and **fails, rolling
  back, if the version it gets differs from the lock**. Packages already at
  the locked version are skipped. `--prune` runs last and only if every
  install succeeded.
- **Pins** — `maca pin <pkg>` holds a package at its installed version:
  `upgrade` (CLI and TUI) skips it. A pin cannot name a different version, and
  an unreadable `pins.json` stops `upgrade` rather than releasing the pins.

## Layout

```
~/.maca/config.toml                 # optional user config
~/.maca/installed.json              # install ledger
~/.maca/custom-registry.json        # custom backend registry
~/.maca/groups/<name>.json          # groups
~/.maca/snapshots/<id>/             # snapshots
~/.maca/cache/brew-index.json       # cached catalog index (1h TTL)
~/.maca/logs/                       # source build logs
~/.maca/prefix/cellar/<name>/<ver>/ # extracted artifacts
~/.maca/prefix/opt/<name>           # Homebrew-style keg symlink
~/.maca/prefix/bin/<exe>            # symlinks into the cellar
~/.maca/prefix/share/...            # symlinks for man pages, info docs, completions
```

## TUI

`maca tui` opens a full-screen interface with four tabs. The screen is a list
plus a detail pane. `?` opens a help overlay.

| Tab       | Keys                                                                                      |
| --------- | ----------------------------------------------------------------------------------------- |
| Installed | `j`/`k` move · `f` kind filter · `p` pin toggle · `u` upgrade · `r` remove · `s` snapshot |
| Search    | `/` edit query · `Enter` search · `i` install                                             |
| Groups    | `g` install group                                                                         |
| Snapshots | `z` restore                                                                               |

`q` or `Ctrl-C` quits. `1`–`4` switch tabs. Remove and snapshot restore ask
for confirmation (`y` to proceed, `n`, `Esc`, or `q` to cancel). While the
help or confirm overlay is open, `q` does not quit. Each mutating action
locks the prefix only while it runs (an upgrade's version check runs before
the lock; a pin toggle does not take a snapshot), so other `maca` commands
work while the TUI is open. If one is running, the action reports "prefix
busy" instead of waiting.

## Self-update

`maca self-update` fetches the latest GitHub release of the configured repo
(`MACA_UPDATE_REPO` or `update_repo` in config, default `donaldfilimon/maca`),
downloads the asset for the host target, verifies its `.sha256` companion,
backs up the current binary, and replaces itself. A release without the
`.sha256` file is refused unless `--force` is given.

Release asset convention: `maca-<target>.tar.gz` plus
`maca-<target>.tar.gz.sha256`, where `<target>` is a Rust target
triple such as `aarch64-apple-darwin`.

On Windows the asset is `maca-<target>.zip` (plus `.sha256`) holding
`maca.exe`. The checksum is verified on the archive before extraction. The
extractor rejects any entry with an absolute or `..` path, takes only
`maca.exe` (one regular file at the archive root or one directory down; more
than one is an error), reads at most 256 MiB of it, and fails with a typed
error if the binary is missing or the archive is corrupt. The `.tar.gz` path
follows the same rules. The running
`maca.exe` is replaced by renaming it to a `maca.bak.<timestamp>` backup and
renaming the new file into place. The repository's release workflow and
`package-release.sh` produce only `.tar.gz` archives today, so no Windows
`.zip` asset exists to download yet.

## Limits (honest scope)

- **Windows installer execution is implemented but unverified.** No Windows
  host was available to execute MSI/EXE installers. The winget catalog,
  manifest resolution, checksum verification, and `--dry-run` plans are
  verified on macOS; the installer launch path is compile-gated to Windows.
- **Self-update is implemented but unverified.** No release is published for
  the default repository yet, so the download-and-replace path has not run
  against a real release.
- **Windows self-update has never run on Windows.** The `.zip` extraction and
  the rename-based replacement helper are platform-independent code that is
  unit-tested on macOS only (zip archives built in the tests). The Windows
  call path that feeds them, and replacing a running `maca.exe`, are
  compile-checked at most (`scripts/cross-check.sh`), not executed.
- **Source builds are best-effort.** Autotools, CMake, Meson, Make, Cargo, and
  Python projects are detected; projects with bespoke build systems fail with a
  clear message and a log path.
- **Homebrew dependency resolution** follows the formula `dependencies` list;
  `uses_from_macos` entries are treated as system-provided.
- **Snapshots cover the packages installed in their prefix** (or only the
  `--only` ones), and restoring one changes nothing outside that scope. On
  APFS they are copy-on-write clones (`cp -c`), so they take no extra space
  until files change; elsewhere they are full recursive copies, so a snapshot
  of a large prefix is not free.
- **One install per package per data dir.** Several prefixes can share one
  `MACA_HOME`, but a package can be installed in only one of them; installing
  it into a second prefix is refused. Use a separate `MACA_HOME` per prefix
  for fully independent installs.
- **Version pinning is verify-only.** A lockfile or group version is checked
  after install, not requested from the catalog.
- **`scripts/package-release.sh` packages the host build only.** Pushing a
  `v<version>` tag runs `.github/workflows/release.yml`, which attaches the
  Linux x86_64 and macOS arm64 archives to the GitHub release. That workflow
  has not run yet, so no release exists for `self-update` to download.

## Development

```sh
./check.sh
```

Runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, a
release build, and `maca doctor`.

See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions, testing, and
how to contribute.

## Security

See [SECURITY.md](SECURITY.md) for reporting vulnerabilities.

## Code of Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## License

Apache-2.0 — see [LICENSE](LICENSE).
