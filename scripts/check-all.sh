#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
python3 -m unittest tools/compatibility-probe/test_probe.py
python3 -m unittest tools/catalog-fixture/test_generator.py

pnpm --dir web install --frozen-lockfile
pnpm --dir web test
pnpm --dir web build
