#!/bin/sh
# Type-check and lint maca for Linux and Windows from a macOS host, the way
# CI's `cargo clippy -D warnings` would, without a Linux or Windows machine.
#
# Needs: rustup with the x86_64-unknown-linux-gnu and x86_64-pc-windows-msvc
# targets, and zig (stands in for a cross C compiler for ring's C files).
# Nothing is linked, so this proves the code compiles and lints for those
# targets, not that it runs there.
set -eu
cd "$(dirname "$0")/.."

command -v zig >/dev/null 2>&1 || { echo "cross-check: zig not found" >&2; exit 1; }
TC="$(rustup run stable rustc --print sysroot)/bin"
TMP="${TMPDIR:-/tmp}/maca-cross-$$"
mkdir -p "$TMP/bin"
trap 'rm -rf "$TMP/bin"' EXIT

# cc-rs passes --target=<rust triple>, which zig cannot parse; drop it.
cat > "$TMP/bin/cc-linux" <<'SH'
#!/bin/sh
for a in "$@"; do shift; case "$a" in --target=*) ;; *) set -- "$@" "$a";; esac; done
exec zig cc -target x86_64-linux-gnu "$@"
SH
# zig's mingw headers stand in for the MSVC SDK (compile only).
cat > "$TMP/bin/cc-win" <<'SH'
#!/bin/sh
for a in "$@"; do shift; case "$a" in --target=*|-nologo|/nologo) ;; *) set -- "$@" "$a";; esac; done
exec zig cc -target x86_64-windows-gnu "$@"
SH
cat > "$TMP/bin/ar" <<'SH'
#!/bin/sh
exec zig ar "$@"
SH
# lib.exe-style arguments (-out:FILE, -nologo) for the MSVC target.
cat > "$TMP/bin/ar-win" <<'SH'
#!/bin/sh
out=""
for a in "$@"; do shift; case "$a" in -out:*) out="${a#-out:}";; -nologo|/nologo) ;; *) set -- "$@" "$a";; esac; done
exec zig ar crs "$out" "$@"
SH
chmod +x "$TMP/bin/"*

export PATH="$TC:$PATH" RUSTC="$TC/rustc"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/cross}"
export CC_x86_64_unknown_linux_gnu="$TMP/bin/cc-linux" AR_x86_64_unknown_linux_gnu="$TMP/bin/ar"
export CC_x86_64_pc_windows_msvc="$TMP/bin/cc-win" AR_x86_64_pc_windows_msvc="$TMP/bin/ar-win"

for target in x86_64-unknown-linux-gnu x86_64-pc-windows-msvc; do
    echo "== cargo clippy --target $target =="
    "$TC/cargo" clippy --target "$target" --all-targets -- -D warnings
done
echo "cross-check.sh: OK (type-check and lint only; nothing was run)"
