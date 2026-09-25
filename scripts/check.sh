#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked -p broxser-desktop --all-targets -- -D warnings
cargo test --locked -p broxser-desktop -j 2
