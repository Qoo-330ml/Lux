#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/restart-lux-cleanup.sh"

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/lux-restart-cleanup.XXXXXX")"
STALE_TARGET="$TEST_ROOT/stale-target"
FRESH_TARGET="$TEST_ROOT/fresh-target"
cleanup() {
    find "$TEST_ROOT" -depth -delete
}
trap cleanup EXIT

mkdir -p "$STALE_TARGET/debug/build" "$FRESH_TARGET/debug/deps"

printf 'Signature: 8a477f597d28d172789f06886806bc55\n' > "$STALE_TARGET/CACHEDIR.TAG"
printf 'old generated build output\n' > "$STALE_TARGET/debug/build/bindgen.rs"
touch -t 202001010000 \
    "$STALE_TARGET/CACHEDIR.TAG" \
    "$STALE_TARGET/debug/build/bindgen.rs"

printf 'Signature: 8a477f597d28d172789f06886806bc55\n' > "$FRESH_TARGET/CACHEDIR.TAG"
printf 'recent dependency output\n' > "$FRESH_TARGET/debug/deps/recent.rmeta"
touch -t "$(date -v-10M '+%Y%m%d%H%M.%S')" "$FRESH_TARGET/debug/deps/recent.rmeta"

cleanup_old_target_if_stale "$STALE_TARGET" 1440 cargo
cleanup_old_target_if_stale "$FRESH_TARGET" 1440 cargo

test ! -e "$STALE_TARGET/debug/build/bindgen.rs"
test -e "$FRESH_TARGET/debug/deps/recent.rmeta"

printf '%s\n' 'restart-lux cleanup test passed'
