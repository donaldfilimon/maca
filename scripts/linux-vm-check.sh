#!/bin/sh
# Run the maca gate on Linux from a macOS host, in an Apple `container` VM,
# and bring back the Linux release archive.
#
#   scripts/linux-vm-check.sh [arm64|amd64]     default: arm64
#   scripts/linux-vm-check.sh rm [arm64|amd64]  delete that VM
#
# The VM gets a clean export of HEAD (`git archive`), so uncommitted changes
# are not tested and nothing in this checkout is written except
# target/dist/maca-<linux triple>.tar.gz and its .sha256. The gate inside is
# the same ./check.sh CI runs, followed by ./scripts/package-release.sh.
# `amd64` runs under Rosetta emulation and is several times slower.
#
# Needs: Apple's `container` tool. First use pulls the image and installs Rust
# in the VM; later runs reuse both.
set -eu
cd "$(dirname "$0")/.."

die() { echo "linux-vm-check: $*" >&2; exit 1; }

ACTION=run
if [ "${1:-}" = rm ]; then ACTION=rm; shift; fi
ARCH=${1:-arm64}
case "$ARCH" in arm64|amd64) ;; *) die "unknown architecture: $ARCH (arm64 or amd64)";; esac

NAME=${MACA_VM:-maca-linux-$ARCH}
IMAGE=${MACA_VM_IMAGE:-ubuntu:24.04}

command -v container >/dev/null 2>&1 || die "Apple's container tool is not installed"
state() { container list --all 2>/dev/null | awk -v n="$NAME" '$1==n {print $5}'; }
in_vm() { container exec "$NAME" sh -c "$1"; }

if [ "$ACTION" = rm ]; then
    [ -n "$(state)" ] || die "no VM named $NAME"
    container stop "$NAME" >/dev/null 2>&1 || true
    container rm "$NAME" >/dev/null
    echo "linux-vm-check: removed $NAME"
    exit 0
fi

container system status >/dev/null 2>&1 || container system start </dev/null >/dev/null 2>&1 \
    || die "container system service would not start"

case "$(state)" in
    running) ;;
    stopped) container start "$NAME" >/dev/null ;;
    *) container run -d --name "$NAME" --arch "$ARCH" -c 4 -m 6G "$IMAGE" sleep infinity >/dev/null ;;
esac
sleep 2

echo "== vm $NAME: toolchain =="
# make: the source-build tests run a real Makefile, as GitHub's runners can.
in_vm 'command -v cc >/dev/null 2>&1 && command -v curl >/dev/null 2>&1 && command -v make >/dev/null 2>&1' || in_vm 'export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq && apt-get install -y -qq --no-install-recommends \
        curl ca-certificates gcc libc6-dev make git >/dev/null'
in_vm '[ -x "$HOME/.cargo/bin/cargo" ]' || in_vm 'curl --proto =https --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain stable -c rustfmt -c clippy >/dev/null 2>&1'
in_vm '. "$HOME/.cargo/env"; uname -m; rustc --version'

echo "== vm $NAME: clean export of $(git rev-parse --short HEAD) =="
# Keep /work/target between runs so a re-run is incremental.
in_vm 'mkdir -p /work && find /work -mindepth 1 -maxdepth 1 ! -name target -exec rm -rf {} +'
git archive HEAD | container exec -i "$NAME" tar -x -C /work

echo "== vm $NAME: ./check.sh =="
in_vm '. "$HOME/.cargo/env"; cd /work && ./check.sh'

echo "== vm $NAME: ./scripts/package-release.sh =="
in_vm '. "$HOME/.cargo/env"; cd /work && rm -rf target/dist && ./scripts/package-release.sh >/dev/null && ls target/dist'

mkdir -p target/dist
container exec "$NAME" tar -c -C /work/target/dist . | tar -x -C target/dist
echo "linux-vm-check: OK ($ARCH); archive copied to target/dist"
