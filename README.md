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
export PATH="$HOME/.maca/prefix/bin:$PATH"
```

Override the prefix with `--prefix` or `MACA_PREFIX`.

## Commands

```sh
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
maca upgrade                       # honours pins
maca remove jq
maca why oniguruma                 # reverse dependencies

maca group create dev
maca group add dev jq --backend brew --kind cli
maca group install dev             # concurrent
maca group export dev              # writes a lockfile
maca lock                          # lockfile from the ledger
maca sync                          # install exactly what the lock pins

maca pin jq                        # pin to the installed version
maca unpin jq

maca snapshot create --note "before upgrade"
maca snapshot list
maca snapshot restore <id>
maca snapshot delete <id>

maca self-update --check
maca self-update
maca tui                           # interactive terminal UI
maca doctor
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
- **Automatic snapshot** before `install`/`remove`/`upgrade` (disable with
  `--no-snapshot`). A failed operation rolls the prefix back automatically.
- **SHA-256 verification** of every artifact against its catalog metadata.
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
- **Groups** — named profiles of packages with optional version pins, installed
  as a unit.
- **Lockfiles** — `<prefix>/maca.lock.json` pins exact versions; `maca sync`
  reproduces a machine.
- **Pins** — `maca pin <pkg>` keeps `upgrade` from touching a package.

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
backs up the current binary, and replaces itself.

Release asset convention: `maca-<target>-<version>.tar.gz` plus
`maca-<target>-<version>.tar.gz.sha256`, where `<target>` is a Rust target
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
- **Snapshots clone directories** (`clonefile` on APFS, recursive copy
  elsewhere), so a snapshot of a large prefix is not free.

## Development

```sh
./check.sh
```

Runs `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, a
release build, and `maca doctor`.
