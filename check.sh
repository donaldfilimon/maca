#!/bin/sh
# maca repo gate: format, lint, test, build.
set -eu

cd "$(dirname "$0")"

# Ensure cargo and rustc are found on standard macOS/Linux developer paths
if ! command -v cargo >/dev/null 2>&1; then
    export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
fi

echo "== cargo fmt --check =="
cargo fmt --check

echo "== cargo clippy =="
cargo clippy --all-targets --all-features -- -D warnings

echo "== cargo test =="
cargo test

echo "== cargo build --release =="
cargo build --release

echo "== maca self-check =="
./target/release/maca --version
./target/release/maca doctor

echo "check.sh: OK"
