#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/restart-lux-cleanup.sh"

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/lux-restart-cleanup.XXXXXX")"
STALE_TARGET="$TEST_ROOT/stale-target"
FRESH_TARGET="$TEST_ROOT/fresh-target"
GIT_TMP_ROOT="$TEST_ROOT/git-tmp"
OPEN_PACK_PID=''
cleanup() {
    if [[ -n "$OPEN_PACK_PID" ]]; then
        kill "$OPEN_PACK_PID" 2>/dev/null || true
        wait "$OPEN_PACK_PID" 2>/dev/null || true
    fi
    find "$TEST_ROOT" -depth -delete
}
trap cleanup EXIT

mkdir -p "$STALE_TARGET/debug/build" "$FRESH_TARGET/debug/deps"
mkdir -p \
    "$GIT_TMP_ROOT/tmp.stale/objects/pack" \
    "$GIT_TMP_ROOT/tmp.fresh/objects/pack" \
    "$GIT_TMP_ROOT/tmp.open/objects/pack"

printf 'Signature: 8a477f597d28d172789f06886806bc55\n' > "$STALE_TARGET/CACHEDIR.TAG"
printf 'old generated build output\n' > "$STALE_TARGET/debug/build/bindgen.rs"
touch -t 202001010000 \
    "$STALE_TARGET/CACHEDIR.TAG" \
    "$STALE_TARGET/debug/build/bindgen.rs"

printf 'Signature: 8a477f597d28d172789f06886806bc55\n' > "$FRESH_TARGET/CACHEDIR.TAG"
printf 'recent dependency output\n' > "$FRESH_TARGET/debug/deps/recent.rmeta"
touch -t "$(date -v-10M '+%Y%m%d%H%M.%S')" "$FRESH_TARGET/debug/deps/recent.rmeta"

printf 'stale git pack\n' > "$GIT_TMP_ROOT/tmp.stale/objects/pack/tmp_pack_stale"
touch -t 202001010000 "$GIT_TMP_ROOT/tmp.stale/objects/pack/tmp_pack_stale"

printf 'fresh git pack\n' > "$GIT_TMP_ROOT/tmp.fresh/objects/pack/tmp_pack_fresh"
touch -t "$(date -v-10M '+%Y%m%d%H%M.%S')" \
    "$GIT_TMP_ROOT/tmp.fresh/objects/pack/tmp_pack_fresh"

printf 'open git pack\n' > "$GIT_TMP_ROOT/tmp.open/objects/pack/tmp_pack_open"
touch -t 202001010000 "$GIT_TMP_ROOT/tmp.open/objects/pack/tmp_pack_open"
sleep 30 < "$GIT_TMP_ROOT/tmp.open/objects/pack/tmp_pack_open" &
OPEN_PACK_PID=$!

cleanup_old_target_if_stale "$STALE_TARGET" 1440 cargo
cleanup_old_target_if_stale "$FRESH_TARGET" 1440 cargo
cleanup_stale_git_temp_packs "$GIT_TMP_ROOT" 60

kill "$OPEN_PACK_PID" 2>/dev/null || true
wait "$OPEN_PACK_PID" 2>/dev/null || true
OPEN_PACK_PID=''

test ! -e "$STALE_TARGET/debug/build/bindgen.rs"
test -e "$FRESH_TARGET/debug/deps/recent.rmeta"
test ! -e "$GIT_TMP_ROOT/tmp.stale/objects/pack/tmp_pack_stale"
test -e "$GIT_TMP_ROOT/tmp.fresh/objects/pack/tmp_pack_fresh"
test -e "$GIT_TMP_ROOT/tmp.open/objects/pack/tmp_pack_open"

printf '%s\n' 'restart-lux cleanup test passed'
