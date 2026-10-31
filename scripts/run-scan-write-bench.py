#!/usr/bin/env python3
"""Scan write benchmark: wall clock and WAL bytes for a full library scan (W4).

The scan write path is the surface W4 changes (search/provider triggers enqueue instead of indexing
inside the caller's transaction), so this benchmark measures one full scan of a library with real
files and reports the job's wall clock, WAL growth and database growth.

Start from a copy of an existing fixture so the admin session and schema are already in place, drop
its catalog rows, then scan a freshly seeded directory:

    python3 scripts/run-scan-write-bench.py \
        --fixture-dir /tmp/lux-rm-resume3 --files 6000 --work-dir /tmp/lux-scan-bench

`--legacy-search-triggers` replaces the enqueue triggers with the pre-W4 inline FTS definitions
before starting the server, so the same fixture can produce the "before" number. Only stdlib is
used. Numbers describe this machine; they are not NAS/x86_64 results.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import socket
import sqlite3
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
COOKIE = "lux_session"
TOKEN = "lux-api-performance-token"
CSRF_TOKEN = "spec-csrf-token"

LEGACY_SEARCH_TRIGGERS = (
    """CREATE TRIGGER media_items_search_ai AFTER INSERT ON media_items BEGIN
        INSERT INTO media_search_map (item_id) VALUES (NEW.id);
        INSERT INTO media_search (rowid, item_id, title, sort_title, original_title, aliases)
        SELECT map.fts_rowid, NEW.id, NEW.title,
               CASE WHEN NEW.sort_title = NEW.title COLLATE NOCASE THEN '' ELSE NEW.sort_title END,
               COALESCE(NEW.original_title, ''), ''
        FROM media_search_map map WHERE map.item_id = NEW.id;
    END""",
    """CREATE TRIGGER media_items_search_au AFTER UPDATE OF title, sort_title, original_title ON media_items
     WHEN OLD.title IS NOT NEW.title
       OR OLD.sort_title IS NOT NEW.sort_title
       OR OLD.original_title IS NOT NEW.original_title
     BEGIN
        DELETE FROM media_search
        WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = OLD.id);
        INSERT INTO media_search (rowid, item_id, title, sort_title, original_title, aliases)
        SELECT map.fts_rowid, NEW.id, NEW.title,
               CASE WHEN NEW.sort_title = NEW.title COLLATE NOCASE THEN '' ELSE NEW.sort_title END,
               COALESCE(NEW.original_title, ''),
               COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.id), '')
        FROM media_search_map map WHERE map.item_id = NEW.id;
    END""",
    """CREATE TRIGGER media_items_search_ad AFTER DELETE ON media_items BEGIN
        DELETE FROM media_search
        WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = OLD.id);
        DELETE FROM media_search_map WHERE item_id = OLD.id;
    END""",
    """CREATE TRIGGER item_aliases_search_ai AFTER INSERT ON item_aliases BEGIN
        UPDATE media_search
        SET aliases = COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.item_id), '')
        WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = NEW.item_id);
    END""",
)
SEARCH_TRIGGER_NAMES = (
    "media_items_search_ai",
    "media_items_search_au",
    "media_items_search_ad",
    "item_aliases_search_ai",
    "item_aliases_search_au",
    "item_aliases_search_ad",
)


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def file_bytes(path: pathlib.Path) -> int:
    return path.stat().st_size if path.exists() else 0


class Server:
    def __init__(self, binary: pathlib.Path, config: pathlib.Path, log: pathlib.Path):
        environment = dict(os.environ)
        environment["LUX_CONFIG_DIR"] = str(config)
        environment["LUX_DISCOVERY_BIND_ADDR"] = "127.0.0.1:0"
        self.port = free_port()
        environment["LUX_HTTP_ADDR"] = f"127.0.0.1:{self.port}"
        self.base = f"http://127.0.0.1:{self.port}"
        self.log = log.open("ab")
        self.process = subprocess.Popen(
            [str(binary)], cwd=str(ROOT), env=environment, stdout=self.log, stderr=self.log
        )

    def wait_ready(self, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"{self.base}/health/ready", timeout=2) as response:
                    if response.status == 200:
                        return
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                pass
            time.sleep(0.25)
        raise SystemExit(f"server did not become ready in {timeout:.0f}s")

    def close(self) -> None:
        self.process.terminate()
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.kill()
        self.log.close()


def reset_catalog(database: pathlib.Path) -> None:
    connection = sqlite3.connect(database)
    try:
        for statement in (
            "DELETE FROM media_items",
            "DELETE FROM libraries WHERE system_kind IS NULL",
            "DELETE FROM catalog_outbox",
            "DELETE FROM search_outbox",
            "DELETE FROM catalog_consumers",
            "DELETE FROM catalog_items",
            "DELETE FROM catalog_item_stats",
            "DELETE FROM user_series_next",
        ):
            try:
                connection.execute(statement)
            except sqlite3.OperationalError:
                pass
        connection.commit()
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    finally:
        connection.close()


def install_legacy_triggers(database: pathlib.Path) -> None:
    connection = sqlite3.connect(database)
    try:
        for name in SEARCH_TRIGGER_NAMES:
            connection.execute(f"DROP TRIGGER IF EXISTS {name}")
        for statement in LEGACY_SEARCH_TRIGGERS:
            connection.execute(statement)
        connection.commit()
    finally:
        connection.close()


def seed_library(database: pathlib.Path, root: pathlib.Path, files: int) -> str:
    root.mkdir(parents=True, exist_ok=True)
    root = root.resolve()
    for index in range(files):
        path = root / f"Scan.Movie.{index:06d}.2024.mkv"
        if not path.exists():
            path.write_bytes(b"scan")
    library_id = f"00000000-0000-7000-8000-{abs(hash(str(root))) % 10**12:012d}"
    connection = sqlite3.connect(database)
    try:
        connection.execute(
            "INSERT INTO libraries (id, name, kind, is_enabled, scan_concurrency, probe_concurrency)"
            " VALUES (?, 'Scan Benchmark', 'MOVIE', 1, 2, 1)",
            (library_id,),
        )
        connection.execute(
            "INSERT INTO library_roots"
            " (id, library_id, canonical_path, display_path, is_available, is_writable)"
            " VALUES (?, ?, ?, ?, 1, 0)",
            (f"{library_id}-root", library_id, str(root), str(root)),
        )
        connection.commit()
    finally:
        connection.close()
    return library_id


def post_scan(base: str, library_id: str, timeout: float) -> dict:
    request = urllib.request.Request(
        f"{base}/api/v1/admin/libraries/{library_id}/scan",
        method="POST",
        headers={
            "Cookie": f"{COOKIE}={TOKEN}",
            "X-CSRF-Token": CSRF_TOKEN,
            "Content-Length": "0",
        },
        data=b"",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read())


class WalSampler:
    """Sums the positive growth of the WAL file, i.e. the bytes appended while sampling.

    A checkpoint truncates or rewrites the file, so the file size alone under-reports the written
    volume; summing positive deltas at a short interval approximates it, and the peak is reported
    separately because it bounds recovery work.
    """

    def __init__(self, wal_path: pathlib.Path):
        self.wal_path = wal_path
        self.appended = 0
        self.peak = file_bytes(wal_path)
        self._last = self.peak
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        while not self._stop.is_set():
            size = file_bytes(self.wal_path)
            if size > self._last:
                self.appended += size - self._last
                self.peak = max(self.peak, size)
            self._last = size
            self._stop.wait(0.01)

    def __enter__(self) -> "WalSampler":
        self._thread.start()
        return self

    def __exit__(self, *_exc) -> None:
        self._stop.set()
        self._thread.join(timeout=5)


def wait_for_items(
    database: pathlib.Path, library_id: str, expected: int, timeout: float
) -> float:
    """Waits for the scan to index `expected` items and returns the elapsed seconds."""
    started = time.monotonic()
    deadline = started + timeout
    while time.monotonic() < deadline:
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True, timeout=5)
        try:
            count = connection.execute(
                "SELECT COUNT(*) FROM media_items WHERE library_id = ?", (library_id,)
            ).fetchone()[0]
        finally:
            connection.close()
        if count >= expected:
            return time.monotonic() - started
        time.sleep(0.05)
    raise SystemExit(f"scan did not index {expected} items in {timeout:.0f}s")


def wait_for_outbox(database: pathlib.Path, timeout: float) -> float:
    """Waits until the queue is empty, i.e. the derived consumers caught up."""
    started = time.monotonic()
    deadline = started + timeout
    while time.monotonic() < deadline:
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True, timeout=5)
        try:
            pending = connection.execute(
                "SELECT (SELECT COUNT(*) FROM catalog_outbox) + (SELECT COUNT(*) FROM search_outbox)"
            ).fetchone()[0]
        finally:
            connection.close()
        if pending == 0:
            return time.monotonic() - started
        time.sleep(0.1)
    return time.monotonic() - started


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, default=ROOT / "target/release/luxd")
    parser.add_argument("--fixture-dir", type=pathlib.Path, required=True)
    parser.add_argument("--work-dir", type=pathlib.Path, required=True)
    parser.add_argument("--files", type=int, default=6000)
    parser.add_argument("--legacy-search-triggers", action="store_true")
    parser.add_argument("--keep-work-dir", action="store_true")
    parser.add_argument("--timeout", type=float, default=900.0)
    parser.add_argument("--label", default="")
    args = parser.parse_args()

    if not args.binary.is_file():
        raise SystemExit(f"luxd binary not found: {args.binary}")

    source = args.fixture_dir.resolve() / "config"
    if not (source / "lux.db").is_file():
        raise SystemExit(f"fixture database not found under {source}")
    work = args.work_dir.resolve()
    if work.exists():
        shutil.rmtree(work)
    work.mkdir(parents=True)
    config = work / "config"
    shutil.copytree(source, config)
    for suffix in ("-wal", "-shm"):
        stale = config / f"lux.db{suffix}"
        if stale.exists():
            stale.unlink()
    database = config / "lux.db"

    reset_catalog(database)
    library_id = seed_library(database, work / "media", args.files)

    # One start to apply migrations and let the derived read models settle before timing the scan.
    warmup = Server(args.binary, config, work / "warmup.log")
    try:
        warmup.wait_ready(args.timeout)
    finally:
        warmup.close()

    # Must run after the migration-applying start: the first connect would otherwise install the
    # enqueue triggers from the migration and silently make both configurations identical.
    if args.legacy_search_triggers:
        install_legacy_triggers(database)

    connection = sqlite3.connect(database)
    try:
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        connection.commit()
    finally:
        connection.close()
    wal_before = file_bytes(pathlib.Path(f"{database}-wal"))
    db_before = file_bytes(database)

    wal_path = pathlib.Path(f"{database}-wal")
    server = Server(args.binary, config, work / "server.log")
    try:
        server.wait_ready(args.timeout)
        started = time.monotonic()
        with WalSampler(wal_path) as sampler:
            job = post_scan(server.base, library_id, args.timeout)
            elapsed = wait_for_items(database, library_id, args.files, args.timeout)
            drain_seconds = wait_for_outbox(database, args.timeout)
        db_after = file_bytes(database)
        job_id = job.get("id")
    finally:
        server.close()

    wal_appended = sampler.appended
    wal_peak = sampler.peak
    connection = sqlite3.connect(database)
    try:
        indexed = connection.execute(
            "SELECT COUNT(*) FROM media_items WHERE library_id = ?", (library_id,)
        ).fetchone()[0]
        fts_rows = connection.execute("SELECT COUNT(*) FROM media_search").fetchone()[0]
        queued = connection.execute(
            "SELECT (SELECT COUNT(*) FROM catalog_outbox) + (SELECT COUNT(*) FROM search_outbox)"
        ).fetchone()[0]
        applied = connection.execute(
            "SELECT applied_seq FROM catalog_consumers WHERE name LIKE 'search_reindex%'"
        ).fetchone()
    finally:
        connection.close()

    report = {
        "label": args.label,
        "legacy_search_triggers": args.legacy_search_triggers,
        "files": args.files,
        "items_indexed": indexed,
        "wall_seconds": round(elapsed, 2),
        "drain_seconds": round(drain_seconds, 2),
        "client_seconds": round(time.monotonic() - started, 2),
        "wal_appended_bytes": wal_appended,
        "wal_peak_bytes": wal_peak,
        "wal_appended_mib": round(wal_appended / (1024 * 1024), 1),
        "wal_peak_mib": round(wal_peak / (1024 * 1024), 1),
        "db_mib": round((db_after - db_before) / (1024 * 1024), 1),
        "fts_rows": fts_rows,
        "outbox_pending": queued,
        "search_watermark": applied[0] if applied else None,
        "scan_job": job_id,
    }
    print(json.dumps(report, indent=2))
    if not args.keep_work_dir:
        shutil.rmtree(work, ignore_errors=True)
    else:
        print(f"work dir kept at {work}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
