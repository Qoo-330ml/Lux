#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/lux-performance.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

python3 tools/catalog-fixture/generate.py \
  "$work_dir/catalog" \
  --files "${LUX_PERF_FILE_COUNT:-60000}" \
  --directories "${LUX_PERF_DIRECTORY_COUNT:-600}"

test_filter="${LUX_PERF_TEST_FILTER:-}"
cargo_args=(--release --locked --test performance)
if [[ -n "$test_filter" ]]; then
  cargo_args+=("$test_filter")
fi

LUX_PERF_MEDIA_ROOT="$work_dir/catalog" \
LUX_PERF_FILE_COUNT="${LUX_PERF_FILE_COUNT:-60000}" \
cargo test "${cargo_args[@]}" -- --ignored --nocapture --test-threads=1
