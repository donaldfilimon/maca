# maca

Cross-platform package manager for **macOS** and **Linux**, written in **Rust 2024**.

maca installs packages from three sources **without shelling out to the native
package manager** for catalog or install work:

| Backend | Catalog | Installs |
| --- | --- | --- |
| `brew` | Homebrew formula JSON API (`formulae.brew.sh`) | Bottles (OCI blobs from GHCR) **or source builds** |
| `winget` | `api.winget.run` search + `microsoft/winget-pkgs` manifests | Windows installers (MSI/EXE) on Windows; dry-run plans everywhere |
| `custom` | Local JSON registry + optional HTTP mirrors | `tar.gz`, single binaries, or source trees with build steps |

`--backend auto` (the default) searches custom → brew → winget and lets the
first backend that can resolve a plan handle the install.

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
  concurrent maca processes cannot race the ledger or cellar.
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
- **Nothing foreign is overwritten**: a `bin/` or `share/` entry that belongs
  to another package or is a file of your own is left in place and reported.
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

`maca tui` opens a full-screen interface with four tabs:

| Tab | Keys |
| --- | --- |
| Installed | `j`/`k` move · `u` upgrade · `r` remove · `s` snapshot |
| Search | `/` edit query · `Enter` search · `i` install |
| Groups | `g` install group |
| Snapshots | `z` restore |

`q` or `Ctrl-C` quits. `1`–`4` switch tabs.

## Self-update

`maca self-update` fetches the latest GitHub release of the configured repo
(`MACA_UPDATE_REPO` or `update_repo` in config, default `donaldfilimon/maca`),
downloads the asset for the host target, verifies its `.sha256` companion,
backs up the current binary, and replaces itself. A release without the
`.sha256` file is refused unless `--force` is given.

Release asset convention: `maca-<target>.tar.gz` plus
`maca-<target>.tar.gz.sha256`, where `<target>` is a Rust target
triple such as `aarch64-apple-darwin`.

## Limits (honest scope)

- **Windows installer execution is implemented but unverified.** No Windows
  host was available to execute MSI/EXE installers. The winget catalog,
  manifest resolution, checksum verification, and `--dry-run` plans are
  verified on macOS; the installer launch path is compile-gated to Windows.
- **Self-update is implemented but unverified.** No release is published for
  the default repository yet, so the download-and-replace path has not run
  against a real release.
- **Source builds are best-effort.** Autotools, CMake, Meson, Make, Cargo, and
  Python projects are detected; projects with bespoke build systems fail with a
  clear message and a log path.
- **Homebrew dependency resolution** follows the formula `dependencies` list;
  `uses_from_macos` entries are treated as system-provided.
- **Snapshots cover every installed package.** On APFS they are copy-on-write
  clones (`cp -c`), so they take no extra space until files change; elsewhere
  they are full recursive copies, so a snapshot of a large prefix is not free.
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
