# maca

Cross-platform package manager for **macOS** and **Linux**, written in **Rust 2024**.

maca installs packages from three sources without shelling out to the native
package manager for catalog work:

| Backend | Catalog | Installs |
| --- | --- | --- |
| `brew` | Homebrew formula JSON API (`formulae.brew.sh`) | Homebrew **bottles** downloaded as OCI blobs from GHCR, SHA-256 verified, extracted into the maca cellar |
| `winget` | `api.winget.run` search + `microsoft/winget-pkgs` manifests | Windows installers (MSI/EXE), gated to Windows hosts |
| `custom` | Local JSON registry + optional HTTP mirrors | `tar.gz` archives or single binaries, SHA-256 verified |

`--backend auto` (the default) searches custom → brew → winget and lets the
first backend that can resolve an install plan handle it.

## Install

```sh
cargo build --release
# binary: target/release/maca
```

Add the prefix bin dir to your `PATH`:

```sh
export PATH="$HOME/Library/Application Support/maca/prefix/bin:$PATH"   # macOS
export PATH="$HOME/.local/share/maca/prefix/bin:$PATH"                 # Linux
```

Override the prefix with `--prefix` or `MACA_PREFIX`.

## Usage

```sh
maca search jq                 # search all backends
maca info jq                   # resolve a package
maca install jq                # install (deps included)
maca install jq --dry-run      # show the plan without installing
maca list                      # installed ledger
maca upgrade                   # upgrade everything
maca upgrade --package jq      # upgrade one package
maca remove jq                 # delete cellar + ledger entry
maca doctor                    # prefix, backends, counts

# custom registry
maca custom add --name hello --version 1.0.0 \
  --url https://example.com/hello.tar.gz --sha256 <hex>
maca custom list
maca custom remove hello
```

## Layout

```
<data>/config.toml                 # optional user config
<data>/installed.json              # install ledger
<data>/custom-registry.json        # custom backend registry
<prefix>/cellar/<name>/<version>/  # extracted artifacts
<prefix>/opt/<name>                # Homebrew-style keg symlink
<prefix>/bin/<exe>                 # symlinks into the cellar
```

`<data>` is `~/Library/Application Support/maca` on macOS and
`~/.local/share/maca` on Linux. `<prefix>` defaults to `<data>/prefix`.

## How the brew backend works

1. `GET https://formulae.brew.sh/api/formula/<name>.json` for metadata,
   `dependencies`, and the bottle file map.
2. Pick the bottle key for the host (`arm64_tahoe`, `x86_64_linux`, …).
3. Fetch an anonymous GHCR token
   (`https://ghcr.io/token?scope=repository:homebrew/core/<name>:pull`).
4. `GET` the OCI blob and verify its SHA-256 against the formula.
5. Extract into the cellar, create `<prefix>/opt/<name>`, install dependencies
   recursively.
6. Rewrite `@@HOMEBREW_CELLAR@@` / `@@HOMEBREW_PREFIX@@` in every Mach-O file
   with `install_name_tool`, then ad-hoc re-sign with `codesign` (Apple Silicon
   requires a valid signature).
7. Link executables into `<prefix>/bin` and record the package in the ledger.

## How the winget backend works

1. `GET https://api.winget.run/v2/packages?query=…` for search and metadata.
2. Resolve the exact `Publisher.Name` id and latest version from the
   `microsoft/winget-pkgs` contents API (the search API has no exact-id
   endpoint).
3. Read `ShortDescription` / `PackageUrl` from the `*.locale.en-US.yaml`
   manifest.
4. Parse the `*.installer.yaml` for `InstallerUrl` + `InstallerSha256`,
   preferring the current architecture and `user` scope.
5. On Windows, download, verify, and run the installer. On macOS/Linux,
   installs are refused; `--dry-run` still prints the resolved plan.

## Custom registry schema

```json
{
  "packages": [
    {
      "name": "hello",
      "version": "1.0.0",
      "description": "Hello world",
      "homepage": "https://example.com",
      "artifacts": {
        "macos-arm64": {
          "url": "https://example.com/hello-1.0.0.tar.gz",
          "sha256": "deadbeef…",
          "kind": "tar.gz"
        }
      }
    }
  ]
}
```

Platform keys: `macos-arm64`, `macos-x64`, `linux-x64`, `linux-arm64`,
`windows-x64`, plus Darwin aliases (`darwin-aarch64`, `darwin-x86_64`) and a
bare OS key (`macos`, `linux`, `windows`) for arch-independent artifacts.
`kind` is `tar.gz` (extracted; `bin/*` linked) or `binary` (copied to `bin/`).

## Limits (honest scope)

- **No source builds.** Formulae without a bottle are refused.
- **No Windows package installation on macOS/Linux.** The winget catalog and
  `--dry-run` plans work everywhere; running MSI/EXE installers requires
  Windows.
- **No `brew`/`winget` binary required** for search, info, or brew installs.
  The winget installer path uses `msiexec` on Windows.
- **Dependency resolution is Homebrew-only** and follows the formula
  `dependencies` list; `uses_from_macos` entries are treated as system
  provided.
- **No rollback.** Removing a package deletes its cellar; there is no snapshot
  or restore.
- **No concurrent-install locking.** One writer per prefix at a time.

## Development

```sh
./check.sh
```

Runs `cargo fmt --check`, `cargo clippy`, `cargo test`, and a release build.
