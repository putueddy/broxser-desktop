#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
cargo fmt --all -- --check
# The SBOM, and a usable license with its text for every shipped crate, must
# stay valid as dependencies change (ADR 0025).
python3 scripts/sbom.py --check
# Script tests: license texts (ADR 0025); Helium manifests, signatures and
# verdicts (ADR 0026).
python3 -m unittest discover -s scripts/tests
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked -p broxser-desktop --all-targets -- -D warnings
cargo test --locked -p broxser-desktop -j 2
