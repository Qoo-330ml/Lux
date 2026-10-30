#!/usr/bin/env bash
# Regenerates the TypeScript bindings for the Lux API DTOs (docs/API-TYPES-SYNC.md).
#
# The generated files live in web/src/lib/api/generated/ and are committed, so this
# script is only needed after changing a `#[cfg_attr(feature = "ts-bindings", ...)]`
# DTO in Rust. `scripts/check-api-types.sh` runs it in a throwaway copy and fails when
# the committed bindings are stale.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# ts-rs reads its configuration from the environment when the export test runs.
#
# * TS_RS_EXPORT_DIR is the base directory for `#[ts(export_to = "...")]`. It is
#   absolute so the platform-independent "web/src/lib/api/generated/" suffix lands
#   in the repository tree regardless of the current directory.
# * TS_RS_LARGE_INT decides how i64/u64 are rendered. ts-rs defaults to `bigint`,
#   which would silently change every timestamp/count field, so it is pinned to
#   `number` to match the JSON encoder (see "已知限制" in docs/API-TYPES-SYNC.md).
export TS_RS_EXPORT_DIR="$ROOT_DIR"
export TS_RS_LARGE_INT="${TS_RS_LARGE_INT:-number}"

CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
if ! command -v "$CARGO" >/dev/null 2>&1; then
  echo "cargo not found at '$CARGO'; set CARGO to the cargo binary" >&2
  exit 1
fi

# `#[ts(export)]` emits one `export_bindings_<type>` test per annotated type, so the
# filter both triggers the export and keeps the rest of the unit tests out of the run.
exec "$CARGO" test --locked --features ts-bindings --lib export_bindings "$@"
