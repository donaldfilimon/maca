#!/bin/sh
# Package maca release artifacts matching self-update convention:
# maca-<target>.tar.gz
# maca-<target>.tar.gz.sha256
set -eu

cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
fi

VERSION=$(cargo metadata --no-deps --format-version 1 | grep -o '"version":"[^"]*"' | head -n 1 | cut -d'"' -f4)

# Detect host target triple if not provided
TARGET="${1:-}"
if [ -z "$TARGET" ]; then
    OS=$(uname -s | tr '[:upper:]' '[:lower:]')
    ARCH=$(uname -m)
    case "$OS-$ARCH" in
        darwin-arm64) TARGET="aarch64-apple-darwin" ;;
        darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
        linux-x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
        linux-aarch64|linux-arm64) TARGET="aarch64-unknown-linux-gnu" ;;
        *) TARGET="${ARCH}-${OS}" ;;
    esac
fi

# This script packages the host build. Labelling a host binary with another
# triple would make self-update install the wrong architecture, so refuse.
HOST=$(rustc -vV | sed -n 's/^host: //p')
if [ "$TARGET" != "$HOST" ]; then
    echo "Error: target ${TARGET} is not this host (${HOST}); cross-packaging is not supported" >&2
    exit 1
fi

echo "Building maca v${VERSION} for ${TARGET}..."
cargo build --release

DIST_DIR="target/dist"
mkdir -p "$DIST_DIR"

STAGE_DIR="target/release-stage"
rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR"

BIN_SRC="target/release/maca"
if [ ! -f "$BIN_SRC" ]; then
    echo "Error: ${BIN_SRC} not found"
    exit 1
fi

cp "$BIN_SRC" "$STAGE_DIR/maca"
chmod 755 "$STAGE_DIR/maca"
if command -v strip >/dev/null 2>&1; then
    strip "$STAGE_DIR/maca" 2>/dev/null || true
fi

ARCHIVE_NAME="maca-${TARGET}.tar.gz"
echo "Creating ${DIST_DIR}/${ARCHIVE_NAME}..."
tar -czf "${DIST_DIR}/${ARCHIVE_NAME}" -C "$STAGE_DIR" maca

# Compute SHA-256
if command -v shasum >/dev/null 2>&1; then
    (cd "$DIST_DIR" && shasum -a 256 "$ARCHIVE_NAME" > "${ARCHIVE_NAME}.sha256")
elif command -v sha256sum >/dev/null 2>&1; then
    (cd "$DIST_DIR" && sha256sum "$ARCHIVE_NAME" > "${ARCHIVE_NAME}.sha256")
else
    echo "Error: neither shasum nor sha256sum found; self-update requires the .sha256 file" >&2
    exit 1
fi

rm -rf "$STAGE_DIR"
echo "Release packaging complete:"
ls -lh "$DIST_DIR"
