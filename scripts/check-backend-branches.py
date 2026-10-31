#!/usr/bin/env python3
"""Keep backend-specific branches behind a registered allowlist (W6).

`docs/LUX-DATA-PATH-PLAN.md` §5 W6: new code must not add `DatabaseBackend::` business branches, and
the existing ones are registered instead of rewritten. `docs/backend-branches.json` records how many
`DatabaseBackend::` occurrences each file currently has, plus why that file is legitimately
backend-aware.

Verify mode (default) fails when:

- a file that has backend branches is not registered (new branch in a new place),
- a registered file gained more branches than recorded,
- the manifest names a file that no longer exists.

Removing branches is always allowed; `--write` records the reduction.

    python3 scripts/check-backend-branches.py            # verify
    python3 scripts/check-backend-branches.py --write    # refresh the manifest
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE_DIR = ROOT / "src"
MANIFEST = ROOT / "docs" / "backend-branches.json"
PATTERN = "DatabaseBackend::"

# Files that are backend-aware by design; the note explains why the branches are legitimate.
NOTES = {
    "src/storage/sql_templates.rs": "SQL dialect templates: SQLite and PostgreSQL spell the same query differently",
    "src/storage/repository.rs": "storage entry points that dispatch whole query families per backend",
    "src/storage/sql_query_benchmarks.rs": "benchmark harness comparing both dialects",
    "src/storage/library_deletion.rs": "cascade/cleanup SQL differs per backend (no shared UNIQUE/cte semantics)",
    "src/storage/catalog.rs": "catalog SQL with backend-specific JSON/tsvector handling",
    "src/storage/database_maintenance.rs": "vacuum/analyze/pragma maintenance is engine specific",
    "src/storage/users.rs": "user SQL with backend-specific upsert forms",
    "src/storage/jobs/scan/manifest/discovery/checkpoint.rs": "manifest checkpoint SQL differs per backend",
    "src/storage/jobs/scan/manifest/diff.rs": "manifest diff SQL differs per backend",
    "src/storage/filesystem.rs": "filesystem entry SQL uses backend-specific conflict targets",
    "src/storage/jobs/scan/manifest/discovery/presence.rs": "presence SQL differs per backend",
    "src/config/database.rs": "configuration to backend enum mapping",
    "src/storage/search_reindex.rs": "PostgreSQL keeps the inline FTS maintenance for now (documented gap)",
}


def scan() -> dict[str, int]:
    counts: dict[str, int] = {}
    for path in sorted(SOURCE_DIR.rglob("*.rs")):
        if path.name.endswith("_tests.rs"):
            continue
        text = path.read_text(encoding="utf-8")
        count = text.count(PATTERN)
        if count:
            counts[str(path.relative_to(ROOT))] = count
    return counts


def build_manifest(counts: dict[str, int], previous: dict) -> dict:
    previous_notes = {entry["path"]: entry.get("note", "") for entry in previous.get("files", [])}
    files = [
        {
            "path": path,
            "count": count,
            "note": previous_notes.get(path) or NOTES.get(path) or "unregistered backend branch",
        }
        for path, count in sorted(counts.items())
    ]
    return {
        "files": files,
        "total": sum(counts.values()),
        "rule": (
            "New code must not add DatabaseBackend:: branches. Growing an existing file requires "
            "editing this manifest in the same commit, which documents the reason in review."
        ),
    }


def verify(counts: dict[str, int], manifest: dict) -> tuple[list[str], list[str]]:
    problems: list[str] = []
    improvements: list[str] = []
    registered = {entry["path"]: entry["count"] for entry in manifest.get("files", [])}
    for path, count in sorted(counts.items()):
        if path not in registered:
            problems.append(f"{path}: {count} unregistered backend branch(es)")
        elif count > registered[path]:
            problems.append(f"{path}: backend branches grew {registered[path]} -> {count}")
        elif count < registered[path]:
            improvements.append(f"{path}: backend branches reduced {registered[path]} -> {count}")
    for path in sorted(set(registered) - set(counts)):
        problems.append(f"{path}: registered but has no backend branches any more")
    return problems, improvements


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="regenerate the manifest")
    args = parser.parse_args()

    counts = scan()
    if args.write:
        previous = json.loads(MANIFEST.read_text()) if MANIFEST.exists() else {}
        manifest = build_manifest(counts, previous)
        MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
        print(
            f"wrote {MANIFEST.relative_to(ROOT)}: {len(counts)} files, "
            f"{sum(counts.values())} branches"
        )
        return 0

    if not MANIFEST.exists():
        print("docs/backend-branches.json is missing; run with --write", file=sys.stderr)
        return 1
    problems, improvements = verify(counts, json.loads(MANIFEST.read_text()))
    for improvement in improvements:
        print(f"backend branches: {improvement} (run --write to record)")
    if problems:
        for problem in problems:
            print(f"backend branches: {problem}", file=sys.stderr)
        return 1
    print(f"backend branches ok: {len(counts)} files, {sum(counts.values())} branches")
    return 0


if __name__ == "__main__":
    sys.exit(main())
