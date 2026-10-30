#!/usr/bin/env bash
# Fails when web/src/lib/api/generated/ is out of sync with the Rust DTOs
# (docs/API-TYPES-SYNC.md).
#
# The check runs in place: it snapshots the committed bindings to a temporary
# directory, deletes the *.ts files in web/src/lib/api/generated/, regenerates them with
# scripts/generate-api-types.sh and compares the two trees. Deleting before generating
# is what makes a renamed or removed DTO visible - ts-rs only ever writes files, so a
# leftover OldName.ts would otherwise stay behind and be reported as a difference that
# cannot be committed, leaving the check red forever.
#
# On failure the regenerated files stay in the working tree so the fix can be reviewed;
# the messages below name the exact `git add` / `git rm` / `rm` commands to run.

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

# The check deletes the files it is about to regenerate, so a stale binding is already
# gone from the working tree by the time it is reported: the remaining action is to stage
# that deletion when Git tracked the file.
print_removal() {
  echo "  $1 (already removed from the worktree)"
  if git ls-files --error-unmatch -- "$1" >/dev/null 2>&1; then
    echo "    stage the deletion with: git rm $1"
  else
    echo "    nothing to stage: Git did not track it"
  fi
}

list_generated_files() {
  (cd "$1" && find . -maxdepth 1 -name '*.ts' -print | sed 's|^\./||' | sort)
}

fail=0

# 1. Snapshot the committed state.
mkdir -p "$TMP_DIR/committed"
cp -R "$GENERATED_DIR/." "$TMP_DIR/committed/"

restore_snapshot() {
  find "$GENERATED_DIR" -maxdepth 1 -name '*.ts' -delete
  cp -R "$TMP_DIR/committed/." "$GENERATED_DIR/"
}

# 2. Start from an empty binding set so the regeneration is the only source of truth.
find "$GENERATED_DIR" -maxdepth 1 -name '*.ts' -delete

# 3. Regenerate.
if ! scripts/generate-api-types.sh > "$TMP_DIR/generate.log" 2>&1; then
  cat "$TMP_DIR/generate.log" >&2
  echo "error: scripts/generate-api-types.sh failed; restored the previous bindings" >&2
  restore_snapshot
  exit 1
fi

list_generated_files "$TMP_DIR/committed" > "$TMP_DIR/committed.list"
list_generated_files "$GENERATED_DIR" > "$TMP_DIR/generated.list"

# 4. Bindings that disappeared (renamed or removed DTO in Rust) or appeared (new DTO).
removed="$(comm -23 "$TMP_DIR/committed.list" "$TMP_DIR/generated.list")"
if [[ -n "$removed" ]]; then
  echo "error: ts-rs no longer generates these committed bindings; delete them:" >&2
  while IFS= read -r name; do
    print_removal "$GENERATED_DIR/$name" >&2
  done <<< "$removed"
  fail=1
fi

added="$(comm -13 "$TMP_DIR/committed.list" "$TMP_DIR/generated.list")"
if [[ -n "$added" ]]; then
  echo "error: these bindings were generated but are not committed; add them:" >&2
  while IFS= read -r name; do
    echo "  git add $GENERATED_DIR/$name" >&2
  done <<< "$added"
  fail=1
fi

# 5. Content drift in the files that exist on both sides.
comm -12 "$TMP_DIR/committed.list" "$TMP_DIR/generated.list" > "$TMP_DIR/common.list"
changed=0
while IFS= read -r name; do
  [[ -n "$name" ]] || continue
  if ! cmp -s "$TMP_DIR/committed/$name" "$GENERATED_DIR/$name"; then
    diff -u \
      --label "$name (committed)" \
      --label "$name (regenerated)" \
      "$TMP_DIR/committed/$name" "$GENERATED_DIR/$name" >&2 || true
    changed=1
  fi
done < "$TMP_DIR/common.list"
if [[ "$changed" -ne 0 ]]; then
  echo "error: $GENERATED_DIR is stale; commit the regenerated bindings" >&2
  fail=1
fi

# 6. ts-rs writes a type that has `#[ts(export)]` but no `export_to` next to the export
# directory base, i.e. at the repository root. Such a file never reaches the Web build,
# so it must be deleted rather than ignored - and it would otherwise make every
# consecutive run fail without saying what to do.
stray="$(find . -maxdepth 1 -name '*.ts' -print | sed 's|^\./||' | sort)"
if [[ -n "$stray" ]]; then
  echo "error: ts-rs wrote bindings outside $GENERATED_DIR (a #[ts(export)] type is missing export_to); delete them:" >&2
  while IFS= read -r name; do
    print_removal "$name" >&2
  done <<< "$stray"
  fail=1
fi

if [[ "$fail" -eq 0 ]]; then
  echo "ok: $GENERATED_DIR matches the Rust DTOs"
fi

exit "$fail"
