#!/bin/sh
# Install a released maca binary for this machine.
#
#   curl -fsSL https://raw.githubusercontent.com/donaldfilimon/maca/main/scripts/install.sh | sh
#
# It downloads maca-<target>.tar.gz and its .sha256 from the GitHub release,
# refuses to continue unless the checksum matches, and puts the one `maca`
# binary in the install directory. Nothing else is written, and nothing is
# run as root.
#
#   MACA_VERSION      release tag to install, e.g. v0.1.0-alpha.1
#                     (default: the latest stable release, or the newest
#                     prerelease when no stable release exists yet)
#   MACA_INSTALL_DIR  where to put the binary (default: $HOME/.local/bin)
#   MACA_REPO         owner/repo to download from (default: donaldfilimon/maca)
set -eu

REPO=${MACA_REPO:-donaldfilimon/maca}
DIR=${MACA_INSTALL_DIR:-$HOME/.local/bin}

die() { echo "install.sh: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }
need curl
need tar

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) TARGET=aarch64-apple-darwin ;;
    Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
    Linux-aarch64|Linux-arm64) TARGET=aarch64-unknown-linux-gnu ;;
    Linux-x86_64) TARGET=x86_64-unknown-linux-gnu ;;
    *) die "no release archive for $(uname -s) $(uname -m); build from source with cargo" ;;
esac

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    die "sha256sum or shasum is required to verify the download"
fi

# First "tag_name" in a GitHub API response, without needing jq.
first_tag() { sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1; }

TAG=${MACA_VERSION:-}
if [ -z "$TAG" ]; then
    API=https://api.github.com/repos/$REPO/releases
    TAG=$(curl -fsSL "$API/latest" 2>/dev/null | first_tag || true)
    if [ -z "$TAG" ]; then
        # No stable release: the list is newest first and includes prereleases.
        TAG=$(curl -fsSL "$API?per_page=1" | first_tag || true)
    fi
    [ -n "$TAG" ] || die "could not find a release of $REPO; set MACA_VERSION"
fi

ASSET=maca-$TARGET.tar.gz
BASE=https://github.com/$REPO/releases/download/$TAG
TMP=$(mktemp -d "${TMPDIR:-/tmp}/maca-install.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

echo "install.sh: downloading $ASSET from $REPO $TAG"
curl -fsSL "$BASE/$ASSET" -o "$TMP/$ASSET" || die "$TAG has no $ASSET"
curl -fsSL "$BASE/$ASSET.sha256" -o "$TMP/$ASSET.sha256" \
    || die "$TAG has no $ASSET.sha256; refusing to install an unverified binary"

WANT=$(cut -d' ' -f1 < "$TMP/$ASSET.sha256")
GOT=$(sha256 "$TMP/$ASSET")
[ -n "$WANT" ] && [ "$WANT" = "$GOT" ] || die "checksum mismatch for $ASSET (expected $WANT, got $GOT)"

mkdir "$TMP/out"
tar -xzf "$TMP/$ASSET" -C "$TMP/out" maca
[ -f "$TMP/out/maca" ] || die "$ASSET did not contain a maca binary"

mkdir -p "$DIR"
chmod 755 "$TMP/out/maca"
# Rename within the directory, so an existing maca is replaced in one step.
mv "$TMP/out/maca" "$DIR/.maca.new.$$"
mv -f "$DIR/.maca.new.$$" "$DIR/maca"

echo "install.sh: installed $("$DIR/maca" --version) to $DIR/maca"
case ":$PATH:" in
    *":$DIR:"*) ;;
    *) echo "install.sh: $DIR is not on PATH; add it, then run: eval \"\$(maca shellenv)\"" ;;
esac
