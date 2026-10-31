#!/usr/bin/env python3
"""Audit `media_items` indexes against the queries that actually use them (W4).

`media_items` is the hottest write target in the schema: the second-round W4 item in
`docs/PERFORMANCE-DB-HANDOFF.md` calls out its index count and trigger count as write
amplification. This script produces the evidence behind `docs/INDEX-AUDIT.md`:

1. it builds a scratch SQLite database by applying `migrations/*.sql` in version order (the same
   files the `sqlx` migrator embeds),
2. it seeds a small but shaped dataset (libraries, movies, series, seasons, episodes; a mix of
   available/unavailable/removed/merged rows) and runs `ANALYZE`,
3. for each entry in the query corpus below it runs `EXPLAIN QUERY PLAN` and records which
   `media_items` indexes the planner picks,
4. it reports every index with no corpus query, every index whose column list is a left prefix of
   another `media_items` index (structural redundancy), and optionally re-explains the corpus after
   dropping a candidate index (`--simulate-drop`) to show which index takes over.

Each corpus entry names the Rust source file it came from plus a `anchor` substring that must still
exist in that file. A missing anchor fails the run, so the corpus cannot silently describe SQL that
the code no longer issues.

Only the Python standard library is used. The query plans are SQLite plans from a scratch database
on this machine; they are not NAS/x86_64 timings, and PostgreSQL plans are not covered here.

    python3 scripts/audit-media-items-indexes.py
    python3 scripts/audit-media-items-indexes.py --simulate-drop idx_media_items_parent_removed
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sqlite3
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SQLITE_DIR = ROOT / "migrations"

# Representative queries, one per distinct access shape that touches `media_items`. `params`
# are literals substituted for the `?` placeholders so the planner sees a normal statement.
CORPUS = [
    {
        "id": "catalog-page",
        "why": "catalog list page (default title order)",
        "source": "src/storage/catalog.rs",
        "anchor": "ORDER BY mi.sort_title, mi.id",
        "params": ["lib-movies", 24, 0],
        "sql": """SELECT mi.id, mi.library_id, mi.item_type, mi.sort_title
                  FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                    AND mi.removed_at IS NULL
                  ORDER BY mi.sort_title, mi.id
                  LIMIT ? OFFSET ?""",
    },
    {
        "id": "catalog-sort-name",
        "why": "catalog list sorted by name (CatalogSort::Name)",
        "source": "src/storage/catalog.rs",
        "anchor": "LOWER(mi.title) ASC, mi.id ASC",
        "params": ["lib-movies", 24],
        "sql": """SELECT mi.id FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                    AND mi.removed_at IS NULL
                  ORDER BY LOWER(mi.title) ASC, mi.id ASC
                  LIMIT ?""",
    },
    {
        "id": "catalog-sort-sortname",
        "why": "catalog list sorted by sort name (CatalogSort::SortName)",
        "source": "src/storage/catalog.rs",
        "anchor": "LOWER(mi.sort_title) ASC, mi.id ASC",
        "params": ["lib-movies", 24],
        "sql": """SELECT mi.id FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                    AND mi.removed_at IS NULL
                  ORDER BY LOWER(mi.sort_title) ASC, mi.id ASC
                  LIMIT ?""",
    },
    {
        "id": "catalog-sort-added",
        "why": "catalog list sorted by date created (CatalogSort::DateCreated desc)",
        "source": "src/storage/catalog.rs",
        "anchor": "mi.added_at DESC, LOWER(mi.title) ASC, mi.id ASC",
        "params": ["lib-movies", 24],
        "sql": """SELECT mi.id FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                    AND mi.removed_at IS NULL
                  ORDER BY mi.added_at DESC, LOWER(mi.title) ASC, mi.id ASC
                  LIMIT ?""",
    },
    {
        "id": "catalog-sort-premiere",
        "why": "catalog list sorted by premiere date (no dedicated index since 0221)",
        "source": "src/storage/catalog.rs",
        "anchor": "COALESCE(NULLIF(mi.premiere_date, ''), CAST(mi.production_year AS TEXT) || '-01-01') DESC",
        "params": ["lib-movies", 24],
        "sql": """SELECT mi.id FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                    AND mi.removed_at IS NULL
                  ORDER BY CASE WHEN NULLIF(mi.premiere_date, '') IS NULL
                                AND mi.production_year IS NULL THEN 1 ELSE 0 END ASC,
                           COALESCE(NULLIF(mi.premiere_date, ''),
                                    CAST(mi.production_year AS TEXT) || '-01-01') DESC,
                           LOWER(mi.title) ASC, mi.id ASC
                  LIMIT ?""",
    },
    {
        "id": "catalog-sort-rating",
        "why": "catalog list sorted by rating (no dedicated index since 0221)",
        "source": "src/storage/catalog.rs",
        "anchor": "CASE WHEN mi.rating IS NULL THEN 1 ELSE 0 END ASC",
        "params": ["lib-movies", 24],
        "sql": """SELECT mi.id FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                    AND mi.removed_at IS NULL
                  ORDER BY CASE WHEN mi.rating IS NULL THEN 1 ELSE 0 END ASC,
                           mi.rating DESC, LOWER(mi.title) ASC, mi.id ASC
                  LIMIT ?""",
    },
    {
        "id": "catalog-count",
        "why": "count_catalog_items (library page header)",
        "source": "src/storage/catalog.rs",
        "anchor": "WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'",
        "params": ["lib-movies"],
        "sql": """SELECT COUNT(*) FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                    AND mi.removed_at IS NULL""",
    },
    {
        "id": "catalog-parent-children-count",
        "why": "season/collection children count (count by parent_id)",
        "source": "src/storage/catalog.rs",
        "anchor": "WHERE mi.parent_id = ? AND mi.item_type IN ({placeholders}) AND mi.removed_at IS NULL",
        "params": ["season-1", "EPISODE"],
        "sql": """SELECT COUNT(*) FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.parent_id = ? AND mi.item_type IN (?)
                    AND mi.removed_at IS NULL""",
    },
    {
        "id": "catalog-series-episodes",
        "why": "series episode list ordered by season/episode",
        "source": "src/storage/catalog.rs",
        "anchor": "ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id",
        "params": ["series-1", 24, 0],
        "sql": """SELECT mi.id FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.series_id = ? AND mi.item_type = 'EPISODE'
                    AND mi.removed_at IS NULL
                  ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id
                  LIMIT ? OFFSET ?""",
    },
    {
        "id": "home-recent-unavailable-series",
        "why": "recent rail: series without an available source",
        "source": "src/storage/catalog.rs",
        "anchor": "AND mi.has_available_source = 0{content_restriction_predicate}",
        "params": ["lib-series", 24],
        "sql": """SELECT mi.id, mi.sort_title FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.library_id = ? AND mi.item_type = 'SERIES'
                    AND mi.removed_at IS NULL
                    AND mi.has_available_source = 0
                  ORDER BY mi.added_at DESC, mi.sort_title, mi.id
                  LIMIT ?""",
    },
    {
        "id": "home-latest-episode-by-parent",
        "why": "recent rail: latest episode per season (parent_id branch)",
        "source": "src/storage/catalog.rs",
        "anchor": "SELECT episode.parent_id AS root_id, MAX(episode.added_at) AS latest_added_at",
        "params": ["season-000-0"],
        "sql": """SELECT MAX(episode.added_at) FROM media_items episode
                  WHERE episode.item_type = 'EPISODE'
                    AND episode.removed_at IS NULL
                    AND episode.has_available_source = 1
                    AND episode.parent_id = ?""",
    },
    {
        "id": "home-recent-available-episodes",
        "why": "recent rail: latest episode per series",
        "source": "src/storage/catalog.rs",
        "anchor": "WHERE child.item_type = 'EPISODE'",
        "params": ["series-1"],
        "sql": """SELECT MAX(child.added_at) FROM media_items child
                  WHERE child.item_type = 'EPISODE'
                    AND child.removed_at IS NULL
                    AND child.has_available_source = 1
                    AND child.series_id = ?""",
    },
    {
        "id": "played-parent-state",
        "why": "sync_played_container_states: episodes of one parent/series",
        "source": "src/storage/sessions.rs",
        "anchor": "AND ((parent_scope.item_type = 'SEASON' AND episode.parent_id = parent_scope.id)",
        "params": ["season-1"],
        "sql": """SELECT episode.id FROM media_items episode
                  WHERE episode.item_type = 'EPISODE'
                    AND episode.removed_at IS NULL
                    AND episode.has_available_source = 1
                    AND episode.parent_id = ?""",
    },
    {
        "id": "series-episode-with-state",
        "why": "user_series_next: episodes of a series with availability",
        "source": "src/storage/user_series_next.rs",
        "anchor": "AND mi.series_id = ?\n                   AND mi.season_number IS NOT NULL",
        "params": ["series-1"],
        "sql": """SELECT mi.id FROM media_items mi
                  WHERE mi.item_type = 'EPISODE'
                    AND mi.series_id = ?
                    AND mi.removed_at IS NULL
                    AND mi.has_available_source = 1""",
    },
    {
        "id": "person-index-scan",
        "why": "list_person_index_item_ids: id-ordered scan of one library",
        "source": "src/storage/people.rs",
        "anchor": "ORDER BY id LIMIT ?",
        "params": ["lib-movies", 500],
        "sql": """SELECT id FROM media_items
                  WHERE library_id = ? AND removed_at IS NULL
                    AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
                  ORDER BY id LIMIT ?""",
    },
    {
        "id": "person-index-count",
        "why": "count_person_index_items",
        "source": "src/storage/people.rs",
        "anchor": "SELECT COUNT(*) FROM media_items",
        "params": ["lib-movies"],
        "sql": """SELECT COUNT(*) FROM media_items
                  WHERE library_id = ? AND removed_at IS NULL
                    AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')""",
    },
    {
        "id": "library-root-counts",
        "why": "count_catalog_root_items_by_library (library + root item types)",
        "source": "src/storage/catalog.rs",
        "anchor": "AND mi.item_type IN ('MOVIE', 'SERIES')",
        "params": ["lib-movies", "lib-series"],
        "sql": """SELECT mi.library_id, COUNT(*) FROM media_items mi
                  JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                  WHERE mi.removed_at IS NULL
                    AND mi.library_id IN (?, ?)
                    AND mi.item_type IN ('MOVIE', 'SERIES')
                  GROUP BY mi.library_id""",
    },
    {
        "id": "identity-key-lookup",
        "why": "provider-id lookup by generated identity key",
        "source": "src/storage/catalog.rs",
        "anchor": "WHERE identity_key = ? AND item_type = 'MOVIE'",
        "params": ["tmdb:movie:603"],
        "sql": """SELECT id FROM media_items
                  WHERE identity_key = ? AND item_type = 'MOVIE'""",
    },
    {
        "id": "merged-into-fk-probe",
        "why": "foreign key ON DELETE SET NULL child probe (no SELECT filters this column alone)",
        "source": "src/storage/media_merge_records.rs",
        "anchor": "SELECT 1 FROM media_items WHERE id = ? AND merged_into_item_id = ?",
        "params": ["item-00002"],
        "sql": """SELECT 1 FROM media_items WHERE merged_into_item_id = ? LIMIT 1""",
    },
    {
        "id": "migration-title-candidates",
        "why": "emby_migration: title candidates for a migration batch",
        "source": "src/storage/emby_migration.rs",
        "anchor": "WHERE item_type = 'MOVIE'",
        "params": [],
        "sql": """SELECT id FROM media_items
                  WHERE item_type = 'MOVIE'
                    AND sort_title = 'migration item'
                    AND removed_at IS NULL
                    AND (production_year IS NULL OR production_year = 2024)""",
    },
    {
        "id": "pending-nfo-collection",
        "why": "global collection reconciliation: pending movies joined to their NFO cache",
        "source": "src/storage/catalog.rs",
        "anchor": "nfo_collection_reference_key IS NULL",
        "params": [50],
        "sql": """WITH pending AS MATERIALIZED (
                      SELECT id FROM media_items
                      WHERE item_type = 'MOVIE'
                        AND removed_at IS NULL
                        AND nfo_collection_reference_key IS NULL
                  )
                  SELECT pending.id FROM pending
                  JOIN media_item_nfo_cache cache ON cache.item_id = pending.id
                  ORDER BY pending.id LIMIT ?""",
    },
    {
        "id": "removed-items-cleanup",
        "why": "database_cleanup / watch_state_archive: removed rows",
        "source": "src/storage/database_cleanup.rs",
        "anchor": "WHERE mi.removed_at IS NOT NULL",
        "params": [],
        "sql": """SELECT mi.id FROM media_items mi WHERE mi.removed_at IS NOT NULL LIMIT 100""",
    },
    {
        "id": "updated-since",
        "why": "catalog filter with min_date_last_saved (Emby incremental refresh)",
        "source": "src/storage/repository.rs",
        "anchor": 'where_clause.push_str(" AND mi.updated_at >= ?");',
        "params": ["lib-movies", 1_703_500_000, 24],
        "sql": """SELECT mi.id FROM media_items mi
                  WHERE mi.library_id = ? AND mi.updated_at >= ?
                    AND mi.item_type <> 'FOLDER'
                    AND mi.removed_at IS NULL
                  ORDER BY mi.sort_title, mi.id
                  LIMIT ?""",
    },
]


def build_database(path: pathlib.Path) -> sqlite3.Connection:
    connection = sqlite3.connect(path)
    for migration in sorted(SQLITE_DIR.glob("*.sql")):
        connection.executescript(migration.read_text(encoding="utf-8"))
    seed(connection)
    connection.execute("ANALYZE")
    connection.commit()
    return connection


# Shape of the seeded dataset. The planner only prefers the narrow partial indexes once the table
# is big enough to matter, so the seed is sized like a small real library rather than a fixture.
MOVIES = 6_000
SERIES = 60
SEASONS_PER_SERIES = 4
EPISODES_PER_SEASON = 12

INSERT_ITEM = (
    "INSERT INTO media_items (id, library_id, item_type, title, sort_title, parent_id, series_id,"
    " identity_key, identification_status, added_at, updated_at, removed_at, has_available_source,"
    " nfo_collection_reference_key)"
    " VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?, ?, ?, ?, ?)"
)


def item(
    item_id: str,
    library_id: str,
    item_type: str,
    title: str,
    added_at: int,
    *,
    parent_id: str | None = None,
    series_id: str | None = None,
    identity_key: str | None = None,
    removed_at: int | None = None,
    available: int = 1,
    nfo_collection_reference_key: str | None = None,
) -> tuple[object, ...]:
    return (
        item_id,
        library_id,
        item_type,
        title,
        title,
        parent_id,
        series_id,
        identity_key,
        added_at,
        added_at,
        removed_at,
        available,
        nfo_collection_reference_key,
    )


def seed(connection: sqlite3.Connection) -> None:
    for library_id, kind in (("lib-movies", "MOVIE"), ("lib-series", "SERIES")):
        connection.execute(
            "INSERT INTO libraries (id, name, kind) VALUES (?, ?, ?)",
            (library_id, library_id, kind),
        )
    rows: list[tuple[object, ...]] = []
    for index in range(MOVIES):
        added_at = 1_700_000_000 + index * 600
        rows.append(
            item(
                f"item-{index:05d}",
                "lib-movies",
                "MOVIE",
                f"Movie {index:05d}",
                added_at,
                identity_key=f"tmdb:movie:{index}",
                removed_at=None if index % 500 else 1_690_000_000,
                available=1 if index % 50 else 0,
                # Most movies already carry a collection reference, so the pending partial index
                # stays small like it is in a reconciled library.
                nfo_collection_reference_key=(
                    None if index % 400 == 0 else f"collection:{index % 97}"
                ),
            )
        )
    for series in range(SERIES):
        series_id = f"series-{series:03d}"
        added_at = 1_700_000_000 + series * 86_400
        rows.append(
            item(
                series_id,
                "lib-series",
                "SERIES",
                f"Series {series:03d}",
                added_at,
                identity_key=f"tmdb:tv:{series}",
                available=1 if series % 7 else 0,
            )
        )
        for season in range(SEASONS_PER_SERIES):
            season_id = f"season-{series:03d}-{season}"
            season_added = added_at + season * 86_400
            rows.append(
                item(
                    season_id,
                    "lib-series",
                    "SEASON",
                    f"Season {season}",
                    season_added,
                    parent_id=series_id,
                    series_id=series_id,
                )
            )
            for episode in range(EPISODES_PER_SEASON):
                rows.append(
                    item(
                        f"episode-{series:03d}-{season}-{episode:02d}",
                        "lib-series",
                        "EPISODE",
                        f"Episode {episode:02d}",
                        season_added + episode * 3_600,
                        parent_id=season_id,
                        series_id=series_id,
                        available=1 if episode % 4 else 0,
                    )
                )
    connection.executemany(INSERT_ITEM, rows)
    # A handful of merged rows so the FK child index (merged_into_item_id) has targets.
    connection.executemany(
        "UPDATE media_items SET merged_into_item_id = 'item-00002' WHERE id = ?",
        [(f"item-{index:05d}",) for index in range(1, 6)],
    )
    connection.commit()


def media_items_indexes(connection: sqlite3.Connection) -> dict[str, str]:
    rows = connection.execute(
        "SELECT name, COALESCE(sql, '') FROM sqlite_master"
        " WHERE type = 'index' AND tbl_name = 'media_items' AND sql IS NOT NULL ORDER BY name"
    ).fetchall()
    return dict(rows)


def explain(connection: sqlite3.Connection, sql: str, params: list[object]) -> list[str]:
    statement = sql.replace("%", "%%")
    for value in params:
        literal = f"'{value}'" if isinstance(value, str) else str(value)
        statement = statement.replace("?", literal, 1)
    plan = connection.execute("EXPLAIN QUERY PLAN " + statement).fetchall()
    return [str(row[-1]) for row in plan]


def indexes_in_plan(plan: list[str], known: set[str]) -> list[str]:
    used = []
    for detail in plan:
        for name in known:
            # Whole-token match: `idx_media_items_library_sort` is a prefix of
            # `idx_media_items_library_sortname_sort`, so a substring test would over-report.
            if re.search(rf"(?<![0-9A-Za-z_]){re.escape(name)}(?![0-9A-Za-z_])", detail):
                if name not in used:
                    used.append(name)
    return used


def index_columns(sql: str) -> tuple[list[str], str]:
    """Split an index definition into its key columns and its partial predicate."""
    body = sql.split("(", 1)[1]
    depth = 1
    columns: list[str] = []
    current = ""
    for character in body:
        if character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
            if depth == 0:
                break
        if character == "," and depth == 1:
            columns.append(" ".join(current.split()))
            current = ""
            continue
        current += character
    if current.strip():
        columns.append(" ".join(current.split()))
    predicate = sql.split("WHERE", 1)[1].strip() if "WHERE" in sql else ""
    return columns, " ".join(predicate.split())


def prefix_redundancy(indexes: dict[str, str]) -> list[tuple[str, str, int]]:
    parsed = {name: index_columns(sql) for name, sql in indexes.items()}
    findings = []
    for name, (columns, predicate) in sorted(parsed.items()):
        for other, (other_columns, other_predicate) in sorted(parsed.items()):
            if name == other or len(other_columns) <= len(columns):
                continue
            if [column.lower() for column in other_columns[: len(columns)]] != [
                column.lower() for column in columns
            ]:
                continue
            # A narrower partial index is not covered when the wider index carries a predicate the
            # narrower one does not imply.
            if other_predicate and other_predicate != predicate:
                continue
            findings.append((name, other, len(columns)))
    return findings


def check_anchors() -> list[str]:
    problems = []
    for entry in CORPUS:
        path = ROOT / entry["source"]
        if not path.exists():
            problems.append(f"{entry['id']}: missing source file {entry['source']}")
            continue
        if entry["anchor"] not in path.read_text(encoding="utf-8"):
            problems.append(
                f"{entry['id']}: anchor not found in {entry['source']}: {entry['anchor']!r}"
            )
    return problems


def run(drop: str | None) -> int:
    problems = check_anchors()
    if problems:
        print("query corpus drifted from the source:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 1
    with tempfile.TemporaryDirectory() as temp_dir:
        database_path = pathlib.Path(temp_dir) / "audit.db"
        connection = build_database(database_path)
        indexes = media_items_indexes(connection)
        known = set(indexes)
        if drop:
            if drop not in indexes:
                print(f"unknown index: {drop}", file=sys.stderr)
                return 1
            connection.execute(f'DROP INDEX "{drop}"')
            connection.execute("ANALYZE")
            indexes = media_items_indexes(connection)
        usage: dict[str, list[str]] = {name: [] for name in indexes}
        plans: dict[str, list[str]] = {}
        for entry in CORPUS:
            plan = explain(connection, entry["sql"], entry["params"])
            plans[entry["id"]] = plan
            for name in indexes_in_plan(plan, set(indexes)):
                usage[name].append(entry["id"])
        print(f"# media_items index audit ({len(indexes)} explicit indexes)")
        print()
        print("| index | used by |")
        print("|---|---|")
        for name in sorted(indexes):
            queries = usage[name]
            print(f"| `{name}` | {', '.join(queries) if queries else '**no corpus query**'} |")
        print()
        print("left-prefix redundant (narrower index fully covered by a wider one):")
        for name, other, width in prefix_redundancy(indexes):
            print(f"  - {name} (first {width} columns) is a prefix of {other}")
        print()
        print("plans:")
        for entry in CORPUS:
            print(f"  [{entry['id']}] {entry['why']} -- {entry['source']}")
            for detail in plans[entry["id"]]:
                print(f"      {detail}")
        connection.close()
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--simulate-drop",
        metavar="INDEX",
        help="drop this media_items index after seeding, then re-explain the corpus",
    )
    arguments = parser.parse_args()
    return run(arguments.simulate_drop)


if __name__ == "__main__":
    raise SystemExit(main())
