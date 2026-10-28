#!/usr/bin/env python3
"""Check that the two migration sets stay paired (W6).

`migrations/` (SQLite) and `migrations-postgres/` (PostgreSQL) evolve independently: SQLite owns FTS5
tables, rowid maps and triggers that have no PostgreSQL counterpart, and PostgreSQL owns extensions
and statement-level trigger functions. Everything else must exist on both sides with the same
name, and the version numbers are allowed to drift only because of those intentionally unpaired
files.

`docs/migration-map.json` is the manifest that records the current pairing. Verify mode (default)
fails when:

- a migration file is missing from the manifest,
- the manifest lists a file that no longer exists,
- a paired entry's version numbers changed,
- a pair that is listed as paired is no longer on both sides (or vice versa).

`--write` regenerates the manifest from the current directories (reasons for unpaired files are
preserved by name when they already exist).

    python3 scripts/check-migration-parity.py            # verify
    python3 scripts/check-migration-parity.py --write    # refresh the manifest
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
SQLITE_DIR = ROOT / "migrations"
POSTGRES_DIR = ROOT / "migrations-postgres"
MANIFEST = ROOT / "docs" / "migration-map.json"

FILE_PATTERN = re.compile(r"^([0-9]+)_(.*)\.sql$")

# Names whose reason cannot be derived automatically; kept so the manifest stays readable.
DEFAULT_SQLITE_ONLY_REASON = "SQLite-only: FTS5 table, rowid map, pragma or SQLite trigger"
DEFAULT_POSTGRES_ONLY_REASON = "PostgreSQL-only: extension, statement-level trigger function or index"


def read_directory(directory: pathlib.Path) -> dict[str, int]:
    entries: dict[str, int] = {}
    for path in sorted(directory.glob("*.sql")):
        match = FILE_PATTERN.match(path.name)
        if match is None:
            raise SystemExit(f"unexpected migration file name: {path.name}")
        version, name = int(match.group(1)), match.group(2)
        if name in entries:
            raise SystemExit(f"duplicate migration name {name!r} in {directory}")
        entries[name] = version
    return entries


def build_manifest(
    sqlite: dict[str, int], postgres: dict[str, int], previous: dict
) -> dict:
    previous_reasons = {
        entry["name"]: entry.get("reason", "")
        for key in ("sqlite_only", "postgres_only")
        for entry in previous.get(key, [])
    }
    pairs = []
    sqlite_only = []
    postgres_only = []
    for name, version in sorted(sqlite.items(), key=lambda item: item[1]):
        if name in postgres:
            pairs.append({"name": name, "sqlite": version, "postgres": postgres[name]})
        else:
            sqlite_only.append(
                {
                    "name": name,
                    "sqlite": version,
                    "reason": previous_reasons.get(name) or DEFAULT_SQLITE_ONLY_REASON,
                }
            )
    for name, version in sorted(postgres.items(), key=lambda item: item[1]):
        if name not in sqlite:
            postgres_only.append(
                {
                    "name": name,
                    "postgres": version,
                    "reason": previous_reasons.get(name) or DEFAULT_POSTGRES_ONLY_REASON,
                }
            )
    forward = [entry["name"] for entry in pairs]
    backward = [entry["name"] for entry in sorted(pairs, key=lambda entry: entry["postgres"])]
    inversions = sum(1 for lhs, rhs in zip(forward, backward) if lhs != rhs)
    return {
        "pairs": pairs,
        "sqlite_only": sqlite_only,
        "postgres_only": postgres_only,
        "ordering": {
            "shared_names": len(forward),
            "rank_mismatches": inversions,
            "note": (
                "Migration histories evolved with different SQLite-only/PostgreSQL-only interleaving, "
                "so the shared names are not in the same relative order. New migrations must keep a "
                "pair adjacent in both directories, which keeps this count from growing."
            ),
        },
    }


def verify(sqlite: dict[str, int], postgres: dict[str, int], manifest: dict) -> list[str]:
    problems: list[str] = []
    listed: dict[str, tuple[str, int]] = {}
    for entry in manifest.get("pairs", []):
        listed[entry["name"]] = ("pair", entry["sqlite"])
    for entry in manifest.get("sqlite_only", []):
        listed[entry["name"]] = ("sqlite_only", entry["sqlite"])
    for entry in manifest.get("postgres_only", []):
        listed[entry["name"]] = ("postgres_only", entry["postgres"])

    for name in sorted(set(sqlite) | set(postgres)):
        if name not in listed:
            problems.append(f"{name}: migration is not in docs/migration-map.json (run --write)")
    for name in sorted(set(listed) - (set(sqlite) | set(postgres))):
        problems.append(f"{name}: listed in docs/migration-map.json but no longer exists")

    for entry in manifest.get("pairs", []):
        name = entry["name"]
        if name not in sqlite or name not in postgres:
            problems.append(f"{name}: recorded as paired but is no longer present on both sides")
            continue
        if sqlite[name] != entry["sqlite"]:
            problems.append(
                f"{name}: SQLite version moved {entry['sqlite']} -> {sqlite[name]}"
            )
        if postgres[name] != entry["postgres"]:
            problems.append(
                f"{name}: PostgreSQL version moved {entry['postgres']} -> {postgres[name]}"
            )
    for entry in manifest.get("sqlite_only", []):
        name = entry["name"]
        if name in postgres:
            problems.append(f"{name}: recorded as SQLite-only but now exists on PostgreSQL too")
    for entry in manifest.get("postgres_only", []):
        name = entry["name"]
        if name in sqlite:
            problems.append(f"{name}: recorded as PostgreSQL-only but now exists on SQLite too")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="regenerate the manifest")
    args = parser.parse_args()

    sqlite = read_directory(SQLITE_DIR)
    postgres = read_directory(POSTGRES_DIR)
    if args.write:
        previous = json.loads(MANIFEST.read_text()) if MANIFEST.exists() else {}
        manifest = build_manifest(sqlite, postgres, previous)
        MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
        print(
            f"wrote {MANIFEST.relative_to(ROOT)}: {len(manifest['pairs'])} pairs, "
            f"{len(manifest['sqlite_only'])} SQLite-only, "
            f"{len(manifest['postgres_only'])} PostgreSQL-only"
        )
        return 0

    if not MANIFEST.exists():
        print("docs/migration-map.json is missing; run with --write", file=sys.stderr)
        return 1
    problems = verify(sqlite, postgres, json.loads(MANIFEST.read_text()))
    if problems:
        for problem in problems:
            print(f"migration parity: {problem}", file=sys.stderr)
        return 1
    manifest = json.loads(MANIFEST.read_text())
    print(
        f"migration parity ok: {len(manifest['pairs'])} paired, "
        f"{len(manifest['sqlite_only'])} SQLite-only, "
        f"{len(manifest['postgres_only'])} PostgreSQL-only"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
