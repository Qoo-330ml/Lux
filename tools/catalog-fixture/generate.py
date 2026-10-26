#!/usr/bin/env python3
"""Generate deterministic local catalog fixtures without embedding media data.

Two fixtures are supported:

* the original on-disk media tree (``root`` plus ``--files``/``--directories``);
* a synthetic SQLite catalog (``--catalog-db``) that can be seeded into a database
  created by ``luxd``.  The catalog fixture keeps the media library read-only and
  points every item at a small pool of real poster files so image responses still
  exercise the real image path.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import secrets
import shutil
import sqlite3
import struct
import time
import uuid
import zlib
from pathlib import Path


FIXTURE_VERSION = "lux-catalog-fixture-v1"
CATALOG_VERSION = "lux-catalog-db-v1"
DEFAULT_FILE_COUNT = 60_000
DEFAULT_DIRECTORY_COUNT = 600
DEFAULT_ITEM_COUNT = 1_000_000
DEFAULT_LIBRARY_COUNT = 3
DEFAULT_POSTER_COUNT = 200
FILE_CONTENT = b"LUX PERF FIXTURE\n"
POSTER_EXTENSIONS = {".jpg", ".jpeg", ".png", ".webp"}
LIBRARY_KINDS = ("MOVIE", "SERIES", "MIXED")
UUID_NAMESPACE = uuid.uuid5(uuid.NAMESPACE_URL, "lux.catalog-fixture")


def fixture_entries(
    root: Path,
    file_count: int = DEFAULT_FILE_COUNT,
    directory_count: int = DEFAULT_DIRECTORY_COUNT,
):
    if file_count < 1:
        raise ValueError("file_count must be positive")
    if directory_count < 1:
        raise ValueError("directory_count must be positive")

    files_per_directory = (file_count + directory_count - 1) // directory_count
    for index in range(file_count):
        bucket = index // files_per_directory
        year = 2000 + index % 100
        path = root / f"bucket-{bucket:04d}" / f"Fixture.Movie.{index:06d}.{year}.mkv"
        yield path


def generate_fixture(
    root: Path,
    file_count: int = DEFAULT_FILE_COUNT,
    directory_count: int = DEFAULT_DIRECTORY_COUNT,
) -> dict[str, object]:
    root = root.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True)
    entries = list(fixture_entries(root, file_count, directory_count))
    for path in entries:
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            path.write_bytes(FILE_CONTENT)
        elif path.read_bytes() != FILE_CONTENT:
            raise ValueError(f"existing fixture file has unexpected content: {path}")

    manifest = {
        "fixtureVersion": FIXTURE_VERSION,
        "fileCount": file_count,
        "directoryCount": directory_count,
        "contentSha256": hashlib.sha256(FILE_CONTENT).hexdigest(),
    }
    (root / ".lux-fixture.json").write_text(
        json.dumps(manifest, ensure_ascii=False, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return manifest


def _stable_uuid(label: str) -> str:
    return str(uuid.uuid5(UUID_NAMESPACE, label))


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _png_dimensions(path: Path) -> tuple[int, int] | None:
    with path.open("rb") as handle:
        header = handle.read(24)
    if len(header) < 24 or header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        return None
    width, height = struct.unpack(">II", header[16:24])
    return int(width), int(height)


def _jpeg_dimensions(path: Path) -> tuple[int, int] | None:
    with path.open("rb") as handle:
        if handle.read(2) != b"\xff\xd8":
            return None
        while True:
            marker = handle.read(2)
            if len(marker) < 2 or marker[0] != 0xFF:
                return None
            code = marker[1]
            if code in (0xD8, 0xD9):
                continue
            length_bytes = handle.read(2)
            if len(length_bytes) < 2:
                return None
            length = struct.unpack(">H", length_bytes)[0]
            if length < 2:
                return None
            if code in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
                payload = handle.read(5)
                if len(payload) < 5:
                    return None
                height, width = struct.unpack(">HH", payload[1:5])
                return int(width), int(height)
            handle.seek(length - 2, os.SEEK_CUR)


def image_dimensions(path: Path) -> tuple[int, int] | None:
    suffix = path.suffix.lower()
    if suffix == ".png":
        return _png_dimensions(path)
    if suffix in (".jpg", ".jpeg"):
        return _jpeg_dimensions(path)
    return None


def _write_placeholder_png(path: Path, index: int, width: int = 960, height: int = 1440) -> None:
    """Write a small deterministic PNG so the fixture never needs bundled images."""

    hue = (index * 47) % 256
    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter type: none
        band = (y * 255) // max(1, height - 1)
        for x in range(0, width, 3):
            rows.extend((hue, (band + x // 7) % 256, (255 - band) % 256))
    compressed = zlib.compress(bytes(rows), level=6)
    chunk = struct.pack(">I", width) + struct.pack(">I", height) + bytes((8, 2, 0, 0, 0))
    payload = bytearray(b"\x89PNG\r\n\x1a\n")
    payload.extend(struct.pack(">I", len(chunk)) + b"IHDR" + chunk + struct.pack(">I", zlib.crc32(b"IHDR" + chunk)))
    payload.extend(struct.pack(">I", len(compressed)) + b"IDAT" + compressed + struct.pack(">I", zlib.crc32(b"IDAT" + compressed)))
    payload.extend(struct.pack(">I", 0) + b"IEND" + struct.pack(">I", zlib.crc32(b"IEND")))
    path.write_bytes(payload)


def poster_pool(
    destination: Path,
    posters_dir: Path | None = None,
    limit: int = DEFAULT_POSTER_COUNT,
    *,
    placeholder_dimensions: tuple[int, int] = (960, 1440),
) -> list[dict[str, object]]:
    """Materialize up to ``limit`` poster files under ``destination``.

    When ``posters_dir`` is provided, the files are hard linked (or copied across
    filesystems) from the given directory so the fixture reuses real image bytes.
    Otherwise deterministic placeholder PNGs are generated.
    """

    if limit < 1:
        raise ValueError("poster limit must be positive")
    destination = destination.expanduser().resolve()
    destination.mkdir(parents=True, exist_ok=True)

    sources: list[Path] = []
    if posters_dir is not None:
        base = posters_dir.expanduser().resolve()
        if not base.is_dir():
            raise ValueError(f"posters directory does not exist: {base}")
        for child in sorted(base.rglob("*")):
            if len(sources) >= limit:
                break
            if child.is_file() and child.suffix.lower() in POSTER_EXTENSIONS:
                sources.append(child)
        if not sources:
            raise ValueError(f"no supported poster files under: {base}")

    posters: list[dict[str, object]] = []
    for index in range(limit):
        source = sources[index % len(sources)] if sources else None
        if source is not None:
            target = destination / f"poster-{index:03d}{source.suffix.lower()}"
            if not target.exists():
                try:
                    os.link(source, target)
                except OSError:
                    shutil.copy2(source, target)
            dimensions = image_dimensions(target)
        else:
            target = destination / f"poster-{index:03d}.png"
            if not target.exists():
                _write_placeholder_png(target, index, *placeholder_dimensions)
            dimensions = image_dimensions(target) or placeholder_dimensions
        if dimensions is None:
            dimensions = placeholder_dimensions
        posters.append(
            {
                "path": target,
                "width": dimensions[0],
                "height": dimensions[1],
                "file_size": target.stat().st_size,
                "content_tag": _sha256(target),
            }
        )
    return posters


def _table_exists(connection: sqlite3.Connection, name: str) -> bool:
    row = connection.execute(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", (name,)
    ).fetchone()
    return row is not None


def _require_catalog_schema(connection: sqlite3.Connection) -> None:
    required = (
        "libraries",
        "library_roots",
        "media_items",
        "item_images",
        "media_sources",
        "media_streams",
        "filesystem_entries",
        "users",
        "web_sessions",
    )
    missing = [name for name in required if not _table_exists(connection, name)]
    if missing:
        raise ValueError(
            "database is not a migrated Lux catalog; missing tables: " + ", ".join(missing)
        )


class _BatchWriter:
    """Buffer rows per table and flush them with executemany.

    Tables are flushed in foreign-key dependency order; a batch triggered by an
    ``item_images`` insert can therefore still safely insert the ``media_items``
    rows that the images reference.
    """

    TABLE_ORDER = (
        "libraries",
        "library_roots",
        "users",
        "web_sessions",
        "media_items",
        "filesystem_entries",
        "media_sources",
        "media_streams",
        "item_images",
        "people",
        "person_credits",
        "user_item_state",
    )

    def __init__(self, connection: sqlite3.Connection, batch_size: int = 2000) -> None:
        self.connection = connection
        self.batch_size = batch_size
        self.rows: dict[tuple[str, tuple[str, ...]], list[tuple[object, ...]]] = {}
        self.pending = 0

    def add(self, table: str, columns: tuple[str, ...], row: tuple[object, ...]) -> None:
        self.rows.setdefault((table, columns), []).append(row)
        self.pending += 1
        if self.pending >= self.batch_size:
            self.flush()

    def flush(self) -> None:
        for table in self.TABLE_ORDER:
            for (pending_table, columns), rows in self.rows.items():
                if pending_table != table:
                    continue
                placeholders = ", ".join("?" for _ in columns)
                try:
                    self.connection.executemany(
                        f"INSERT INTO {table} ({', '.join(columns)}) VALUES ({placeholders})",
                        rows,
                    )
                except sqlite3.Error as error:
                    raise sqlite3.IntegrityError(f"{table} insert failed: {error}") from error
        self.rows.clear()
        self.pending = 0


def _item_type(kind: str, index: int, library_count: int) -> str:
    if kind == "SERIES":
        return "SERIES"
    if kind == "MOVIE":
        return "MOVIE"
    return "MOVIE" if (index // max(1, library_count)) % 2 == 0 else "SERIES"


def seed_catalog(
    database: Path,
    media_root: Path,
    *,
    item_count: int = DEFAULT_ITEM_COUNT,
    library_count: int = DEFAULT_LIBRARY_COUNT,
    poster_limit: int = DEFAULT_POSTER_COUNT,
    posters_dir: Path | None = None,
    session_token: str | None = None,
    now: int | None = None,
    batch_size: int = 2000,
    search_index: bool = True,
    detail_items: int | None = None,
) -> dict[str, object]:
    """Seed a synthetic catalog into an empty, migrated Lux SQLite database."""

    if item_count < 1:
        raise ValueError("item_count must be positive")
    detail_item_count = item_count if detail_items is None else max(0, min(item_count, detail_items))
    if not 1 <= library_count <= len(LIBRARY_KINDS):
        raise ValueError(f"library_count must be between 1 and {len(LIBRARY_KINDS)}")

    database = database.expanduser().resolve()
    media_root = media_root.expanduser().resolve()
    if not database.is_file():
        raise ValueError(f"database does not exist: {database}")
    media_root.mkdir(parents=True, exist_ok=True)
    timestamp = int(time.time()) if now is None else int(now)
    token = session_token or secrets.token_urlsafe(32)

    connection = sqlite3.connect(database, timeout=60)
    try:
        connection.execute("PRAGMA foreign_keys = ON")
        connection.execute("PRAGMA journal_mode = WAL")
        connection.execute("PRAGMA synchronous = OFF")
        connection.execute("PRAGMA cache_size = -200000")
        connection.execute("PRAGMA temp_store = MEMORY")
        _require_catalog_schema(connection)
        if connection.execute("SELECT COUNT(*) FROM media_items").fetchone()[0] > 0:
            raise ValueError("catalog database already contains media items")
        if (
            connection.execute(
                "SELECT COUNT(*) FROM libraries WHERE system_kind IS NULL"
            ).fetchone()[0]
            > 0
        ):
            raise ValueError("catalog database already contains libraries")

        posters = poster_pool(
            media_root / "posters", posters_dir, poster_limit
        )

        search_insert_trigger = None
        if not search_index:
            row = connection.execute(
                "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'media_items_search_ai'"
            ).fetchone()
            search_insert_trigger = row[0] if row else None
            connection.execute("DROP TRIGGER IF EXISTS media_items_search_ai")

        writer = _BatchWriter(connection, batch_size=batch_size)
        with connection:
            libraries: list[dict[str, object]] = []
            for index in range(library_count):
                kind = LIBRARY_KINDS[index]
                library_id = _stable_uuid(f"library:{index}")
                library_root = media_root
                libraries.append(
                    {
                        "id": library_id,
                        "name": f"Perf {kind.title()}",
                        "kind": kind,
                    }
                )
                writer.add(
                    "libraries",
                    ("id", "name", "kind", "is_enabled", "scan_concurrency", "probe_concurrency"),
                    (library_id, f"Perf {kind.title()}", kind, 1, 2, 1),
                )
                writer.add(
                    "library_roots",
                    (
                        "id",
                        "library_id",
                        "canonical_path",
                        "display_path",
                        "is_available",
                        "is_writable",
                    ),
                    (_stable_uuid(f"root:{index}"), library_id, str(library_root), str(library_root), 1, 0),
                )

            user_id = _stable_uuid("user:performance")
            writer.add(
                "users",
                (
                    "id",
                    "username_normalized",
                    "display_name",
                    "password_hash",
                    "is_admin",
                    "can_manage_server",
                    "can_download",
                    "has_password",
                ),
                (user_id, "performance", "Performance", "", 1, 1, 1, 0),
            )
            writer.add(
                "web_sessions",
                ("id", "user_id", "session_token_hash", "csrf_token_hash", "expires_at"),
                (
                    _stable_uuid("session:performance"),
                    user_id,
                    hashlib.sha256(token.encode("utf-8")).digest(),
                    hashlib.sha256(b"perf-csrf-token").digest(),
                    timestamp + 7 * 24 * 60 * 60,
                ),
            )

            sources = 0
            streams = 0
            images = 0
            for index in range(item_count):
                library = libraries[index % library_count]
                kind = str(library["kind"])
                item_type = _item_type(kind, index, library_count)
                item_id = _stable_uuid(f"item:{index}")
                title = f"Perf {item_type.title()} {index:07d}"
                year = 1990 + index % 35
                month = 1 + index % 12
                day = 1 + index % 28
                writer.add(
                    "media_items",
                    (
                        "id",
                        "library_id",
                        "item_type",
                        "title",
                        "sort_title",
                        "overview",
                        "production_year",
                        "premiere_date",
                        "runtime_ticks",
                        "rating",
                        "identification_status",
                        "added_at",
                        "has_available_source",
                    ),
                    (
                        item_id,
                        library["id"],
                        item_type,
                        title,
                        title,
                        f"Performance fixture item {index}.",
                        year,
                        f"{year:04d}-{month:02d}-{day:02d}",
                        5_400_000_000 + (index % 60) * 60_000_000,
                        round((index % 97) / 10.0, 1),
                        "LOCAL_CONFIRMED",
                        timestamp - (index % 1_000_000),
                        1,
                    ),
                )

                if index >= detail_item_count:
                    if writer.pending >= writer.batch_size:
                        writer.flush()
                    continue

                source_count = 1 + (1 if index % 5 == 0 else 0)
                for source_index in range(source_count):
                    file_entry_id = _stable_uuid(f"file:{index}:{source_index}")
                    source_id = _stable_uuid(f"source:{index}:{source_index}")
                    writer.add(
                        "filesystem_entries",
                        (
                            "id",
                            "library_root_id",
                            "relative_path",
                            "entry_kind",
                            "size",
                            "modified_at",
                            "last_seen_generation",
                        ),
                        (
                            file_entry_id,
                            _stable_uuid(f"root:{index % library_count}"),
                            f"media/{item_id}/source-{source_index}.mkv",
                            "FILE",
                            1_000_000 + index % 9_000_000,
                            timestamp - index % 1_000_000,
                            "perf",
                        ),
                    )
                    writer.add(
                        "media_sources",
                        (
                            "id",
                            "item_id",
                            "source_kind",
                            "filesystem_entry_id",
                            "container",
                            "size",
                            "duration_ticks",
                            "bitrate",
                            "is_default",
                            "probe_status",
                        ),
                        (
                            source_id,
                            item_id,
                            "LOCAL_FILE",
                            file_entry_id,
                            "mkv",
                            1_000_000 + index % 9_000_000,
                            5_400_000_000 + (index % 60) * 60_000_000,
                            8_000_000,
                            1 if source_index == 0 else 0,
                            "PROBED",
                        ),
                    )
                    sources += 1
                    stream_count = 3 + (index + source_index) % 4
                    for stream_index in range(stream_count):
                        stream_type = (
                            "VIDEO"
                            if stream_index == 0
                            else "AUDIO"
                            if stream_index % 2 == 1
                            else "SUBTITLE"
                        )
                        codec = (
                            "h264"
                            if stream_type == "VIDEO"
                            else "aac"
                            if stream_type == "AUDIO"
                            else "subrip"
                        )
                        writer.add(
                            "media_streams",
                            (
                                "id",
                                "media_source_id",
                                "stream_index",
                                "stream_type",
                                "codec",
                                "language",
                                "is_default",
                            ),
                            (
                                _stable_uuid(f"stream:{index}:{source_index}:{stream_index}"),
                                source_id,
                                stream_index,
                                stream_type,
                                codec,
                                "jpn" if stream_index % 2 else "eng",
                                1 if stream_index == 0 else 0,
                            ),
                        )
                        streams += 1

                image_types = ("POSTER", "FANART") if index % 2 else ("POSTER", "FANART", "THUMB")
                for image_offset, image_type in enumerate(image_types):
                    poster = posters[(index + image_offset) % len(posters)]
                    writer.add(
                        "item_images",
                        (
                            "id",
                            "item_id",
                            "image_type",
                            "image_index",
                            "local_path",
                            "width",
                            "height",
                            "file_size",
                            "content_tag",
                            "source",
                        ),
                        (
                            _stable_uuid(f"image:{index}:{image_type}"),
                            item_id,
                            image_type,
                            0,
                            str(poster["path"]),
                            poster["width"],
                            poster["height"],
                            poster["file_size"],
                            poster["content_tag"],
                            "LOCAL",
                        ),
                    )
                    images += 1

            writer.flush()
            if search_insert_trigger:
                connection.execute(search_insert_trigger)

            manifest = {
                "catalogVersion": CATALOG_VERSION,
                "fixtureVersion": FIXTURE_VERSION,
                "itemCount": item_count,
                "libraryCount": library_count,
                "sourceCount": sources,
                "streamCount": streams,
                "imageCount": images,
                "posterCount": len(posters),
                "detailItemCount": detail_item_count,
                "postersDirectory": str(media_root / "posters"),
                "mediaRoot": str(media_root),
                "database": str(database),
                "sessionToken": token,
                "libraries": libraries,
                "searchIndexPopulated": search_index,
                "generatedAt": timestamp,
            }
    finally:
        connection.close()

    return manifest



SPEC_TARGET_VERSION = "lux-spec-target-v1"
SPEC_TARGET_OVERVIEW = (
    "A deterministic performance-fixture overview used to give list and detail payloads a "
    "realistic text length without copying third-party metadata. "
)


def _spec_library(writer: "_BatchWriter", *, key: str, name: str, kind: str, media_root: Path) -> str:
    library_id = _stable_uuid(f"spec:library:{key}")
    writer.add(
        "libraries",
        ("id", "name", "kind", "is_enabled", "scan_concurrency", "probe_concurrency"),
        (library_id, name, kind, 1, 2, 1),
    )
    writer.add(
        "library_roots",
        (
            "id",
            "library_id",
            "canonical_path",
            "display_path",
            "is_available",
            "is_writable",
        ),
        (
            _stable_uuid(f"spec:root:{key}"),
            library_id,
            str(media_root),
            str(media_root),
            1,
            0,
        ),
    )
    return library_id


def seed_spec_target(
    database: Path,
    media_root: Path,
    *,
    movies: int = 10_000,
    series_count: int = 1_000,
    episodes_per_series: int = 50,
    seasons_per_series: int = 5,
    detail_items: int = 2_000,
    user_states: int = 2_000,
    actor_items: int = 1_000,
    actors_per_item: int = 5,
    people_count: int = 200,
    poster_limit: int = DEFAULT_POSTER_COUNT,
    posters_dir: Path | None = None,
    session_token: str | None = None,
    now: int | None = None,
    batch_size: int = 2_000,
) -> dict[str, object]:
    """Seed the specification-scale hierarchy fixture (10k movies / 1k series / 50k episodes).

    The fixture is deterministic for a fixed set of counts: item ids, titles, timestamps and
    poster assignments never depend on randomness or the wall clock. ``contentSha256`` hashes the
    generated ``(item_type, id)`` pairs so two runs with the same counts can be compared without
    reading the whole database.
    """

    if movies < 0 or series_count < 0 or movies + series_count < 1:
        raise ValueError("movies and series_count must be non-negative with at least one item")
    if episodes_per_series < 1 or seasons_per_series < 1:
        raise ValueError("episodes_per_series and seasons_per_series must be positive")
    if episodes_per_series < seasons_per_series:
        raise ValueError("episodes_per_series must be at least seasons_per_series")
    if people_count < 1 or actors_per_item < 1:
        raise ValueError("people_count and actors_per_item must be positive")

    database = database.expanduser().resolve()
    media_root = media_root.expanduser().resolve()
    if not database.is_file():
        raise ValueError(f"database does not exist: {database}")
    media_root.mkdir(parents=True, exist_ok=True)
    timestamp = int(time.time()) if now is None else int(now)
    token = session_token or secrets.token_urlsafe(32)

    episode_season_size = (episodes_per_series + seasons_per_series - 1) // seasons_per_series
    season_ids: dict[tuple[int, int], str] = {}
    detail_budget = max(0, detail_items)
    detail_order: list[tuple[str, str]] = []

    connection = sqlite3.connect(database, timeout=60)
    try:
        connection.execute("PRAGMA foreign_keys = ON")
        connection.execute("PRAGMA journal_mode = WAL")
        connection.execute("PRAGMA synchronous = OFF")
        connection.execute("PRAGMA cache_size = -200000")
        connection.execute("PRAGMA temp_store = MEMORY")
        _require_catalog_schema(connection)
        if connection.execute("SELECT COUNT(*) FROM media_items").fetchone()[0] > 0:
            raise ValueError("catalog database already contains media items")
        if (
            connection.execute(
                "SELECT COUNT(*) FROM libraries WHERE system_kind IS NULL"
            ).fetchone()[0]
            > 0
        ):
            raise ValueError("catalog database already contains libraries")

        posters = poster_pool(media_root / "posters", posters_dir, poster_limit)
        writer = _BatchWriter(connection, batch_size=batch_size)
        with connection:
            movie_library = _spec_library(
                writer,
                key="movies",
                name="Spec Movies",
                kind="MOVIE",
                media_root=media_root,
            )
            series_library = _spec_library(
                writer,
                key="series",
                name="Spec Series",
                kind="SERIES",
                media_root=media_root,
            )
            user_id = _stable_uuid("spec:user:performance")
            writer.add(
                "users",
                (
                    "id",
                    "username_normalized",
                    "display_name",
                    "password_hash",
                    "is_admin",
                    "can_manage_server",
                    "can_download",
                    "has_password",
                ),
                (user_id, "performance", "Performance", "", 1, 1, 1, 0),
            )
            writer.add(
                "web_sessions",
                ("id", "user_id", "session_token_hash", "csrf_token_hash", "expires_at"),
                (
                    _stable_uuid("spec:session:performance"),
                    user_id,
                    hashlib.sha256(token.encode("utf-8")).digest(),
                    hashlib.sha256(b"spec-csrf-token").digest(),
                    timestamp + 7 * 24 * 60 * 60,
                ),
            )

            def add_item(
                *,
                item_id: str,
                library_id: str,
                item_type: str,
                title: str,
                added_at: int,
                parent_id: str | None = None,
                series_id: str | None = None,
                season_number: int | None = None,
                episode_number: int | None = None,
                sort_title: str | None = None,
                runtime_ticks: int = 5_400_000_000,
            ) -> None:
                writer.add(
                    "media_items",
                    (
                        "id",
                        "library_id",
                        "item_type",
                        "parent_id",
                        "series_id",
                        "season_number",
                        "episode_number",
                        "title",
                        "sort_title",
                        "overview",
                        "production_year",
                        "premiere_date",
                        "runtime_ticks",
                        "rating",
                        "identification_status",
                        "added_at",
                        "has_available_source",
                    ),
                    (
                        item_id,
                        library_id,
                        item_type,
                        parent_id,
                        series_id,
                        season_number,
                        episode_number,
                        title,
                        sort_title or title,
                        SPEC_TARGET_OVERVIEW * 3,
                        1990 + added_at % 35,
                        f"{1990 + added_at % 35:04d}-01-01",
                        runtime_ticks,
                        round((added_at % 97) / 10.0, 1),
                        "LOCAL_CONFIRMED",
                        added_at,
                        1,
                    ),
                )

                for image_offset, image_type in enumerate(("POSTER", "FANART")):
                    poster = posters[(added_at + image_offset) % len(posters)]
                    writer.add(
                        "item_images",
                        (
                            "id",
                            "item_id",
                            "image_type",
                            "image_index",
                            "local_path",
                            "width",
                            "height",
                            "file_size",
                            "content_tag",
                            "source",
                        ),
                        (
                            _stable_uuid(f"spec:image:{item_id}:{image_type}"),
                            item_id,
                            image_type,
                            0,
                            str(poster["path"]),
                            poster["width"],
                            poster["height"],
                            poster["file_size"],
                            poster["content_tag"],
                            "LOCAL",
                        ),
                    )

            sources = 0
            streams = 0
            for index in range(movies):
                item_id = _stable_uuid(f"spec:movie:{index:06d}")
                add_item(
                    item_id=item_id,
                    library_id=movie_library,
                    item_type="MOVIE",
                    title=f"Spec Movie {index:06d}",
                    added_at=timestamp - index * 60,
                )
                detail_order.append((item_id, movie_library))

            for series_index in range(series_count):
                series_id = _stable_uuid(f"spec:series:{series_index:04d}")
                add_item(
                    item_id=series_id,
                    library_id=series_library,
                    item_type="SERIES",
                    title=f"Spec Series {series_index:04d}",
                    added_at=timestamp - series_index * 600,
                )
                for season_number in range(1, seasons_per_series + 1):
                    season_id = _stable_uuid(f"spec:season:{series_index:04d}:{season_number}")
                    season_ids[(series_index, season_number)] = season_id
                    add_item(
                        item_id=season_id,
                        library_id=series_library,
                        item_type="SEASON",
                        title=f"Spec Series {series_index:04d} Season {season_number:02d}",
                        added_at=timestamp - series_index * 600 - season_number,
                        parent_id=series_id,
                        series_id=series_id,
                        season_number=season_number,
                    )
                for episode_number in range(1, episodes_per_series + 1):
                    season_number = min(
                        seasons_per_series,
                        (episode_number - 1) // episode_season_size + 1,
                    )
                    episode_id = _stable_uuid(
                        f"spec:episode:{series_index:04d}:{episode_number}"
                    )
                    add_item(
                        item_id=episode_id,
                        library_id=series_library,
                        item_type="EPISODE",
                        title=(
                            f"Spec Series {series_index:04d} "
                            f"S{season_number:02d}E{episode_number:02d}"
                        ),
                        added_at=timestamp - series_index * 600 - episode_number,
                        parent_id=season_ids[(series_index, season_number)],
                        series_id=series_id,
                        season_number=season_number,
                        episode_number=episode_number,
                        runtime_ticks=2_700_000_000,
                    )
                    detail_order.append((episode_id, series_library))

            for detail_index, (item_id, library_id) in enumerate(detail_order[:detail_budget]):
                file_entry_id = _stable_uuid(f"spec:file:{item_id}")
                source_id = _stable_uuid(f"spec:source:{item_id}")
                writer.add(
                    "filesystem_entries",
                    (
                        "id",
                        "library_root_id",
                        "relative_path",
                        "entry_kind",
                        "size",
                        "modified_at",
                        "last_seen_generation",
                    ),
                    (
                        file_entry_id,
                        _stable_uuid(
                            "spec:root:movies"
                            if library_id == movie_library
                            else "spec:root:series"
                        ),
                        f"media/{item_id}/source.mkv",
                        "FILE",
                        1_000_000 + detail_index,
                        timestamp - detail_index,
                        "spec",
                    ),
                )
                writer.add(
                    "media_sources",
                    (
                        "id",
                        "item_id",
                        "source_kind",
                        "filesystem_entry_id",
                        "container",
                        "size",
                        "duration_ticks",
                        "bitrate",
                        "is_default",
                        "probe_status",
                    ),
                    (
                        source_id,
                        item_id,
                        "LOCAL_FILE",
                        file_entry_id,
                        "mkv",
                        1_000_000 + detail_index,
                        5_400_000_000,
                        8_000_000,
                        1,
                        "PROBED",
                    ),
                )
                sources += 1
                for stream_index in range(3):
                    writer.add(
                        "media_streams",
                        (
                            "id",
                            "media_source_id",
                            "stream_index",
                            "stream_type",
                            "codec",
                            "language",
                            "is_default",
                        ),
                        (
                            _stable_uuid(f"spec:stream:{item_id}:{stream_index}"),
                            source_id,
                            stream_index,
                            "VIDEO" if stream_index == 0 else "AUDIO",
                            "h264" if stream_index == 0 else "aac",
                            "jpn" if stream_index % 2 else "eng",
                            1 if stream_index == 0 else 0,
                        ),
                    )
                    streams += 1

            episode_ids = [
                _stable_uuid(f"spec:episode:{series_index:04d}:{episode_number}")
                for series_index in range(series_count)
                for episode_number in range(1, episodes_per_series + 1)
            ]
            state_count = min(user_states, len(episode_ids))
            for state_index, item_id in enumerate(episode_ids[:state_count]):
                writer.add(
                    "user_item_state",
                    (
                        "user_id",
                        "item_id",
                        "position_ticks",
                        "is_played",
                        "is_favorite",
                        "play_count",
                        "last_played_at",
                        "version",
                    ),
                    (
                        user_id,
                        item_id,
                        1_000_000 + state_index * 1_000,
                        0,
                        1 if state_index % 5 == 0 else 0,
                        1,
                        timestamp - state_index * 60,
                        1,
                    ),
                )

            person_count = min(people_count, max(1, actor_items * actors_per_item))
            people_ids = []
            for person_index in range(person_count):
                person_id = _stable_uuid(f"spec:person:{person_index:04d}")
                people_ids.append(person_id)
                display_name = f"Spec Performer {person_index:04d}"
                writer.add(
                    "people",
                    (
                        "id",
                        "display_name",
                        "directory_name",
                        "normalized_name",
                        "status",
                        "created_at",
                        "updated_at",
                    ),
                    (
                        person_id,
                        display_name,
                        display_name.lower().replace(" ", "-"),
                        display_name.lower(),
                        "ACTIVE",
                        timestamp,
                        timestamp,
                    ),
                )

            credits = 0
            credit_items = detail_order[:actor_items]
            for item_index, (item_id, _library_id) in enumerate(credit_items):
                for credit_index in range(actors_per_item):
                    person_index = (item_index + credit_index * 7) % len(people_ids)
                    person_id = people_ids[person_index]
                    writer.add(
                        "person_credits",
                        (
                            "item_id",
                            "person_id",
                            "person_type",
                            "person_name",
                            "provider",
                            "role",
                            "sort_order",
                            "lux_person_id",
                        ),
                        (
                            item_id,
                            f"tmdb-{person_index:05d}",
                            "Actor",
                            f"Spec Performer {person_index:04d}",
                            "tmdb",
                            "Actor",
                            credit_index,
                            person_id,
                        ),
                    )
                    credits += 1

            writer.flush()

            item_rows = connection.execute(
                "SELECT item_type, id FROM media_items ORDER BY item_type, id"
            ).fetchall()
            digest = hashlib.sha256()
            for item_type, item_id in item_rows:
                digest.update(f"{item_type}:{item_id}\n".encode("utf-8"))
            counts = {
                item_type: count
                for item_type, count in connection.execute(
                    "SELECT item_type, COUNT(*) FROM media_items GROUP BY item_type"
                ).fetchall()
            }
            manifest = {
                "catalogVersion": CATALOG_VERSION,
                "fixtureVersion": SPEC_TARGET_VERSION,
                "profile": "spec-target",
                "movieCount": counts.get("MOVIE", 0),
                "seriesCount": counts.get("SERIES", 0),
                "seasonCount": counts.get("SEASON", 0),
                "episodeCount": counts.get("EPISODE", 0),
                "itemCount": sum(counts.values()),
                "sourceCount": sources,
                "streamCount": streams,
                "imageCount": connection.execute(
                    "SELECT COUNT(*) FROM item_images"
                ).fetchone()[0],
                "userStateCount": state_count,
                "creditCount": credits,
                "detailItemCount": min(detail_budget, len(detail_order)),
                "contentSha256": digest.hexdigest(),
                "database": str(database),
                "mediaRoot": str(media_root),
                "postersDirectory": str(media_root / "posters"),
                "sessionToken": token,
                "generatedAt": timestamp,
            }
    finally:
        connection.close()

    return manifest

def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", type=Path, help="directory to populate")
    parser.add_argument("--files", type=int)
    parser.add_argument("--directories", type=int, default=DEFAULT_DIRECTORY_COUNT)
    parser.add_argument("--catalog-db", type=Path, help="migrated Lux SQLite database to seed")
    parser.add_argument("--items", type=int, default=DEFAULT_ITEM_COUNT)
    parser.add_argument("--libraries", type=int, default=DEFAULT_LIBRARY_COUNT)
    parser.add_argument("--posters", type=int, default=DEFAULT_POSTER_COUNT)
    parser.add_argument("--posters-dir", type=Path, help="reuse real images from this directory")
    parser.add_argument("--media-root", type=Path, help="media root for the catalog fixture")
    parser.add_argument("--session-token", help="fixed session token for the fixture user")
    parser.add_argument(
        "--skip-search-index",
        action="store_true",
        help="skip the media_search FTS trigger for very large fixtures",
    )
    parser.add_argument("--manifest", type=Path, help="write the combined manifest to this path")
    parser.add_argument(
        "--detail-items",
        type=int,
        help="materialize sources, streams and images for only the first N items",
    )
    parser.add_argument(
        "--spec-target",
        action="store_true",
        help="seed the specification hierarchy profile (movies + series/season/episode)",
    )
    parser.add_argument("--movies", type=int, default=10_000)
    parser.add_argument("--series", dest="series_count", type=int, default=1_000)
    parser.add_argument("--episodes-per-series", type=int, default=50)
    parser.add_argument("--seasons-per-series", type=int, default=5)
    parser.add_argument("--user-states", type=int, default=2_000)
    parser.add_argument("--actor-items", type=int, default=1_000)
    parser.add_argument("--actors-per-item", type=int, default=5)
    parser.add_argument("--people", type=int, default=200)
    return parser


def main() -> int:
    args = _parser().parse_args()
    if args.spec_target:
        if args.catalog_db is None:
            raise SystemExit("--spec-target requires --catalog-db")
        media_root = args.media_root or args.root or args.catalog_db.parent / "media"
        manifest = seed_spec_target(
            args.catalog_db,
            media_root,
            movies=args.movies,
            series_count=args.series_count,
            episodes_per_series=args.episodes_per_series,
            seasons_per_series=args.seasons_per_series,
            detail_items=args.detail_items if args.detail_items is not None else 2_000,
            user_states=args.user_states,
            actor_items=args.actor_items,
            actors_per_item=args.actors_per_item,
            people_count=args.people,
            poster_limit=args.posters,
            posters_dir=args.posters_dir,
            session_token=args.session_token,
            batch_size=2_000,
        )
        if args.manifest is not None:
            args.manifest.write_text(
                json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
        print(json.dumps(manifest, sort_keys=True))
        return 0
    if args.catalog_db is not None:
        media_root = args.media_root or args.root or args.catalog_db.parent / "media"
        manifest = seed_catalog(
            args.catalog_db,
            media_root,
            item_count=args.items,
            library_count=args.libraries,
            poster_limit=args.posters,
            posters_dir=args.posters_dir,
            session_token=args.session_token,
            search_index=not args.skip_search_index,
            detail_items=args.detail_items,
        )
        if args.manifest is not None:
            args.manifest.write_text(
                json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
        print(json.dumps(manifest, sort_keys=True))
        return 0

    if args.root is None:
        raise SystemExit("root is required unless --catalog-db is used")
    file_count = args.files if args.files is not None else DEFAULT_FILE_COUNT
    manifest = generate_fixture(args.root, file_count, args.directories)
    print(json.dumps(manifest, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
