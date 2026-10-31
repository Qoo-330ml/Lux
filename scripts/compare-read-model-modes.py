#!/usr/bin/env python3
"""Interleaved A/B for the read-model switch (`LUX_READ_MODEL_MODE=off|on`).

Single-shot measurements drift with machine load, so this script alternates modes over several
rounds from the same fixture and reports per-endpoint medians plus byte equality. It copies the
fixture, lets the first start apply migrations and finish the read-model backfill, then measures.

Example:

    python3 scripts/compare-read-model-modes.py \
        --work-dir /tmp/lux-rm-resume3 --rounds 3 --requests 20

Only stdlib is used. Numbers describe this machine and fixture; they are not NAS/x86_64 results.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import socket
import sqlite3
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[1]
COOKIE = "lux_session"
TOKEN = "lux-api-performance-token"
# The fixture generator seeds the admin session with this CSRF token (sha256 of it is stored).
CSRF_TOKEN = "spec-csrf-token"
ENDPOINTS = (
    ("home", "/api/v1/home"),
    ("continue-watching", "/api/v1/continue-watching?pageSize=24"),
    ("libraries", "/api/v1/libraries?pageSize=100"),
)


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def fetch(base: str, path: str, timeout: float) -> tuple[bytes, float]:
    request = urllib.request.Request(
        f"{base}{path}", headers={"Cookie": f"{COOKIE}={TOKEN}"}
    )
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=timeout) as response:
        body = response.read()
    return body, (time.perf_counter() - started) * 1000.0


class Server:
    def __init__(self, binary: pathlib.Path, config: pathlib.Path, mode: str, log: pathlib.Path):
        self.mode = mode
        environment = dict(os.environ)
        environment["LUX_CONFIG_DIR"] = str(config)
        environment["LUX_READ_MODEL_MODE"] = mode
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
        raise SystemExit(f"server did not become ready in {timeout:.0f}s (mode {self.mode})")

    def close(self) -> None:
        self.process.terminate()
        try:
            self.process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self.process.kill()
        self.log.close()


def wait_for_backfill(database: pathlib.Path, name: str, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True, timeout=5)
            try:
                # Watermarks are version scoped (`name@<schema version>`), so match the prefix.
                done = connection.execute(
                    "SELECT COUNT(*) FROM catalog_consumers WHERE name = ? OR name LIKE ?",
                    (name, f"{name}@%"),
                ).fetchone()[0]
            finally:
                connection.close()
            if done:
                return
        except sqlite3.Error:
            pass
        time.sleep(0.5)
    raise SystemExit(f"read-model backfill '{name}' did not finish in {timeout:.0f}s")


def reset_derived(database: pathlib.Path) -> None:
    connection = sqlite3.connect(database)
    try:
        for statement in (
            "DELETE FROM catalog_items",
            "DELETE FROM catalog_item_stats",
            "DELETE FROM catalog_consumers",
            "DELETE FROM user_series_next",
        ):
            try:
                connection.execute(statement)
            except sqlite3.OperationalError:
                pass
        connection.commit()
        connection.execute("ANALYZE")
        connection.commit()
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    finally:
        connection.close()


def seed_scan_libraries(
    database: pathlib.Path, media_root: pathlib.Path, libraries: int, files: int
) -> list[str]:
    """Create movie libraries with real files so a scan can run during the measurement.

    The 66k fixture only carries database rows; scanning those roots would mark every item as
    missing. These libraries are added after the prepare start so the read model covers them too.
    """
    library_ids = []
    connection = sqlite3.connect(database)
    try:
        for index in range(libraries):
            root = media_root / f"library-{index}"
            root.mkdir(parents=True, exist_ok=True)
            # Store the resolved path: on macOS /tmp is a symlink and the scanner compares the
            # walked path against the canonical root.
            root = root.resolve()
            for file_index in range(files):
                path = root / f"Scan.Movie.{index:02d}.{file_index:06d}.2024.mkv"
                if not path.exists():
                    path.write_bytes(b"scan")
            library_id = str(uuid.uuid5(uuid.NAMESPACE_URL, f"lux-scan-library:{root}"))
            root_id = str(uuid.uuid5(uuid.NAMESPACE_URL, f"lux-scan-root:{root}"))
            connection.execute(
                "INSERT OR IGNORE INTO libraries"
                " (id, name, kind, is_enabled, scan_concurrency, probe_concurrency)"
                " VALUES (?, ?, 'MOVIE', 1, 2, 1)",
                (library_id, f"Scan Target {index}"),
            )
            connection.execute(
                "INSERT OR IGNORE INTO library_roots"
                " (id, library_id, canonical_path, display_path, is_available, is_writable)"
                " VALUES (?, ?, ?, ?, 1, 0)",
                (root_id, library_id, str(root), str(root)),
            )
            library_ids.append(library_id)
        connection.commit()
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    finally:
        connection.close()
    return library_ids


def post_scan(base: str, library_id: str, timeout: float) -> None:
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
        response.read()


def wait_for_scan_items(
    database: pathlib.Path, library_id: str, expected: int, timeout: float
) -> None:
    deadline = time.monotonic() + timeout
    previous = -1
    stable = 0
    while time.monotonic() < deadline:
        try:
            connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True, timeout=5)
            try:
                count = connection.execute(
                    "SELECT COUNT(*) FROM media_items WHERE library_id = ?", (library_id,)
                ).fetchone()[0]
            finally:
                connection.close()
        except sqlite3.Error:
            time.sleep(0.5)
            continue
        if count >= expected:
            return
        stable = stable + 1 if count == previous else 0
        if stable >= 20 and previous > 0:
            raise SystemExit(
                f"scan of {library_id} stopped at {count}/{expected} items; rerun with fewer files"
            )
        previous = count
        time.sleep(0.5)
    raise SystemExit(f"scan of {library_id} did not finish in {timeout:.0f}s")


def measure_round(base: str, path: str, warmup: int, requests: int, timeout: float):
    for _ in range(warmup):
        fetch(base, path, timeout)
    samples = []
    digest = ""
    size = 0
    for _ in range(requests):
        body, elapsed = fetch(base, path, timeout)
        digest = hashlib.sha256(body).hexdigest()
        size = len(body)
        samples.append(elapsed)
    samples.sort()
    return {
        "p50": statistics.median(samples),
        "p95": samples[min(len(samples) - 1, int(len(samples) * 0.95))],
        "bytes": size,
        "sha256": digest,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, default=ROOT / "target/release/luxd")
    parser.add_argument(
        "--work-dir",
        type=pathlib.Path,
        required=True,
        help="directory whose config/lux.db is the fixture to copy",
    )
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--requests", type=int, default=20)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--startup-timeout", type=float, default=240.0)
    parser.add_argument("--json-out", type=pathlib.Path)
    parser.add_argument("--keep-work-dir", action="store_true")
    parser.add_argument(
        "--scan-libraries",
        type=int,
        default=0,
        help="seed this many real-file movie libraries and re-scan them during each round",
    )
    parser.add_argument(
        "--scan-files",
        type=int,
        default=2000,
        help="tiny media files per scan library",
    )
    args = parser.parse_args()

    source = args.work_dir.resolve() / "config"
    if not (source / "lux.db").is_file():
        raise SystemExit(f"fixture database not found under {source}")
    work = pathlib.Path(f"/tmp/lux-read-model-ab-{os.getpid()}")
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

    # Drop any derived rows the fixture carries so both modes are compared against read models this
    # build actually produced, then let the prepare start rebuild them.
    reset_derived(database)

    # Prepare: apply migrations and wait for both derived read models before timing anything.
    prepare = Server(args.binary, config, "off", work / "prepare.log")
    try:
        prepare.wait_ready(args.startup_timeout)
        wait_for_backfill(database, "catalog_items_rebuild", args.startup_timeout)
        wait_for_backfill(database, "user_series_next_rebuild", args.startup_timeout)
        scan_libraries = []
        if args.scan_libraries > 0:
            # The 66k fixture only carries database rows, so a scan of it would mark everything as
            # missing. Seed separate libraries with real files instead: the first scan fills them,
            # later scans only re-walk, which gives write/CPU pressure without changing content.
            scan_libraries = seed_scan_libraries(
                database, work / "scan-media", args.scan_libraries, args.scan_files
            )
            for library_id in scan_libraries:
                post_scan(prepare.base, library_id, args.timeout)
            for library_id in scan_libraries:
                wait_for_scan_items(database, library_id, args.scan_files, args.startup_timeout)
    finally:
        prepare.close()

    if scan_libraries:
        # The seed scan added items after the read model had been rebuilt, so rebuild once more:
        # otherwise the `on` page would miss the new rows and fall back on every request.
        reset_derived(database)
        rebuild = Server(args.binary, config, "off", work / "rebuild.log")
        try:
            rebuild.wait_ready(args.startup_timeout)
            wait_for_backfill(database, "catalog_items_rebuild", args.startup_timeout)
            wait_for_backfill(database, "user_series_next_rebuild", args.startup_timeout)
        finally:
            rebuild.close()

    labels = list(ENDPOINTS)
    if scan_libraries:
        labels.append(("home-during-scan", "/api/v1/home"))

    rounds = []
    for round_index in range(args.rounds):
        for mode in ("off", "on"):
            server = Server(args.binary, config, mode, work / f"server-{mode}.log")
            try:
                server.wait_ready(args.startup_timeout)
                time.sleep(1.0)
                results = {}
                for label, path in ENDPOINTS:
                    results[label] = measure_round(
                        server.base, path, args.warmup, args.requests, args.timeout
                    )
                if scan_libraries:
                    for library_id in scan_libraries:
                        post_scan(server.base, library_id, args.timeout)
                    results["home-during-scan"] = measure_round(
                        server.base, "/api/v1/home", args.warmup, args.requests, args.timeout
                    )
            finally:
                server.close()
            rounds.append({"round": round_index + 1, "mode": mode, "results": results})

    summary = {}
    for label, _ in labels:
        per_mode = {}
        for mode in ("off", "on"):
            samples = [entry["results"][label] for entry in rounds if entry["mode"] == mode]
            digests = [s["sha256"] for s in samples]
            per_mode[mode] = {
                "p50_median": round(statistics.median([s["p50"] for s in samples]), 2),
                "p95_median": round(statistics.median([s["p95"] for s in samples]), 2),
                "p95_min": round(min(s["p95"] for s in samples), 2),
                "p95_max": round(max(s["p95"] for s in samples), 2),
                "bytes": samples[0]["bytes"],
                "digests": sorted(set(digests)),
            }
        summary[label] = per_mode

    for label, _ in labels:
        digests = {entry["results"][label]["sha256"] for entry in rounds}
        summary[label]["identical_across_modes"] = len(digests) == 1
    report = {"endpoints": summary, "rounds": rounds}
    if args.json_out:
        args.json_out.write_text(json.dumps(report, indent=2))
    print(json.dumps(summary, indent=2))
    if not args.keep_work_dir:
        shutil.rmtree(work, ignore_errors=True)
    else:
        print(f"work dir kept at {work}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
