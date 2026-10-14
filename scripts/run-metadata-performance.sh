#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if ! mount | grep -Fq ' on /Volumes/Toshiba ('; then
  echo "Toshiba is not mounted at /Volumes/Toshiba; refusing to build in a worktree-local target directory." >&2
  exit 1
fi

export CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target

benchmark="${1:-concurrency}"
case "$benchmark" in
  concurrency)
    test_filter=lux_531_metadata_concurrency_benchmark
    ;;
  legacy)
    test_filter=lux_200_metadata_pipeline_benchmark
    ;;
  *)
    echo "usage: $0 [concurrency|legacy]" >&2
    exit 2
    ;;
esac

LUX_METADATA_BENCHMARK_RUSTC_VERSION="$(rustc --version)" cargo test --release --locked \
  --features performance-benchmarks \
  --test performance \
  "$test_filter" \
  -- \
  --ignored \
  --nocapture \
  --test-threads=1
