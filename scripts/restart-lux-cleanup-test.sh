#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/restart-lux-cleanup.sh"

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/lux-restart-cleanup.XXXXXX")"
cleanup() {
    find "$TEST_ROOT" -depth -delete
}
trap cleanup EXIT

mkdir -p \
    "$TEST_ROOT/debug/deps" \
    "$TEST_ROOT/debug/incremental/session" \
    "$TEST_ROOT/debug/build" \
    "$TEST_ROOT/debug/.fingerprint"

printf 'old dependency\n' > "$TEST_ROOT/debug/deps/old.o"
printf 'recent dependency\n' > "$TEST_ROOT/debug/deps/recent.o"
printf 'within-day dependency\n' > "$TEST_ROOT/debug/deps/within-day.o"
printf 'old incremental object\n' > "$TEST_ROOT/debug/incremental/session/old.o"
printf 'old build output\n' > "$TEST_ROOT/debug/build/old.out"
printf 'recent build output\n' > "$TEST_ROOT/debug/build/recent.out"
printf 'old fingerprint\n' > "$TEST_ROOT/debug/.fingerprint/old.json"
printf 'recent fingerprint\n' > "$TEST_ROOT/debug/.fingerprint/recent.json"

touch -t 202001010000 \
    "$TEST_ROOT/debug/deps/old.o" \
    "$TEST_ROOT/debug/incremental/session/old.o" \
    "$TEST_ROOT/debug/build/old.out" \
    "$TEST_ROOT/debug/.fingerprint/old.json"
touch -t "$(date -v-10M '+%Y%m%d%H%M.%S')" "$TEST_ROOT/debug/deps/within-day.o"

cleanup_old_debug_artifacts "$TEST_ROOT"

test ! -e "$TEST_ROOT/debug/deps/old.o"
test ! -e "$TEST_ROOT/debug/incremental/session/old.o"
test ! -e "$TEST_ROOT/debug/build/old.out"
test ! -e "$TEST_ROOT/debug/.fingerprint/old.json"
test -e "$TEST_ROOT/debug/deps/recent.o"
test -e "$TEST_ROOT/debug/deps/within-day.o"
test -e "$TEST_ROOT/debug/build/recent.out"
test -e "$TEST_ROOT/debug/.fingerprint/recent.json"

printf '%s\n' 'restart-lux cleanup test passed'
