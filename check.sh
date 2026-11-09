#!/bin/sh
# maca repo gate: format, lint, test, build.
set -eu

cd "$(dirname "$0")"

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
