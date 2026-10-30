#!/usr/bin/env bash
# Fails when web/src/lib/api/generated/ is out of sync with the Rust DTOs.
#
# Regenerates the bindings with `scripts/generate-api-types.sh` and compares the
# result byte for byte with the committed files (docs/API-TYPES-SYNC.md). When the
# check fails the freshly generated files are left in place so the change can be
# reviewed and committed.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

GENERATED_DIR="web/src/lib/api/generated"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

if [[ ! -d "$GENERATED_DIR" ]]; then
  echo "error: $GENERATED_DIR is missing; run scripts/generate-api-types.sh" >&2
  exit 1
fi

cp -R "$GENERATED_DIR" "$TMP_DIR/committed"

if ! scripts/generate-api-types.sh > "$TMP_DIR/generate.log" 2>&1; then
  cat "$TMP_DIR/generate.log" >&2
  echo "error: scripts/generate-api-types.sh failed" >&2
  exit 1
fi

status=0
if ! diff -ru "$TMP_DIR/committed" "$GENERATED_DIR"; then
  echo "error: $GENERATED_DIR is stale; commit the regenerated bindings" >&2
  status=1
fi

# ts-rs writes a type that has `#[ts(export)]` but no `export_to` next to the export
# directory base, i.e. at the repository root. Catch that instead of silently
# generating an untracked file that never reaches the Web build.
stray="$(find . -maxdepth 1 -name '*.ts' -print)"
if [[ -n "$stray" ]]; then
  echo "error: ts-rs exported types outside $GENERATED_DIR (missing export_to?):" >&2
  echo "$stray" >&2
  status=1
fi

exit "$status"
