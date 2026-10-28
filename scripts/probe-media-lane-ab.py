#!/usr/bin/env python3
"""Probe whether holding back the background media lane protects the foreground page.

`scripts/compare-read-model-modes.py` cannot answer that: it seeds scan libraries during its prepare
phase, so the measurement-time "re-scan" only re-walks directories and the media lane (probe, NFO,
image, thumbnail) has no work to hold back. This probe seeds a library with files the scanner has
never seen, measures `/api/v1/home` while that library is actually being scanned, and prints the
"scan / idle" p95 ratio. Run it once per arm and compare:

    python3 scripts/probe-media-lane-ab.py --throttle off --keep-work-dir
    python3 scripts/probe-media-lane-ab.py --throttle on --keep-work-dir

Only stdlib is used. It is an experiment harness, not a gate check: numbers describe this machine
and this fixture (read model off, 66k catalog) and are not NAS/x86_64 results.
"""

from __future__ import annotations

import argparse
import os
import pathlib
import shutil
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.request
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[1]
COOKIE = "lux_session"
TOKEN = "lux-api-performance-token"
CSRF_TOKEN = "spec-csrf-token"


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def fetch(base: str, path: str, timeout: float = 120.0) -> tuple[bytes, float]:
    request = urllib.request.Request(
        f"{base}{path}", headers={"Cookie": f"{COOKIE}={TOKEN}"}
    )
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=timeout) as response:
        body = response.read()
    return body, (time.perf_counter() - started) * 1000.0


def percentile95(samples: list[float]) -> float:
    ordered = sorted(samples)
    return ordered[max(0, int(len(ordered) * 0.95) - 1)]


def median(samples: list[float]) -> float:
    ordered = sorted(samples)
    return ordered[len(ordered) // 2]


def scan_item_count(database: pathlib.Path, library_id: str) -> int:
    try:
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True, timeout=5)
        try:
            return connection.execute(
                "SELECT COUNT(*) FROM media_items WHERE library_id = ?", (library_id,)
            ).fetchone()[0]
        finally:
            connection.close()
    except sqlite3.Error:
        return 0


def seed_unscanned_library(
    database: pathlib.Path, media_root: pathlib.Path, files: int
) -> str:
    """Create a movie library with real files and no catalog rows, so the scan has work to do."""
    root = media_root / "library-0"
    root.mkdir(parents=True, exist_ok=True)
    # Store the resolved path: on macOS /tmp is a symlink and the scanner compares the walked path
    # against the canonical root.
    root = root.resolve()
    for index in range(files):
        (root / f"Media.Lane.{index:06d}.2024.mkv").write_bytes(b"scan")
    library_id = str(uuid.uuid5(uuid.NAMESPACE_URL, f"lux-scan-library:{root}"))
    root_id = str(uuid.uuid5(uuid.NAMESPACE_URL, f"lux-scan-root:{root}"))
    connection = sqlite3.connect(database)
    try:
        connection.execute(
            "INSERT OR IGNORE INTO libraries"
            " (id, name, kind, is_enabled, scan_concurrency, probe_concurrency)"
            " VALUES (?, ?, 'MOVIE', 1, 4, 4)",
            (library_id, "Media Lane Probe"),
        )
        connection.execute(
            "INSERT OR IGNORE INTO library_roots"
            " (id, library_id, canonical_path, display_path, is_available, is_writable)"
            " VALUES (?, ?, ?, ?, 1, 0)",
            (root_id, library_id, str(root), str(root)),
        )
        connection.commit()
    finally:
        connection.close()
    return library_id


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--throttle", choices=("off", "on"), required=True)
    parser.add_argument("--binary", type=pathlib.Path, default=ROOT / "target/release/luxd")
    parser.add_argument(
        "--work-dir",
        type=pathlib.Path,
        required=True,
        help="directory whose config/lux.db is the fixture to copy",
    )
    parser.add_argument("--threshold-ms", type=int, default=100)
    parser.add_argument("--files", type=int, default=1200)
    parser.add_argument("--requests", type=int, default=30)
    parser.add_argument("--budget", type=int, default=9)
    parser.add_argument("--keep-work-dir", action="store_true")
    args = parser.parse_args()

    source = args.work_dir.resolve() / "config"
    if not (source / "lux.db").is_file():
        raise SystemExit(f"fixture database not found under {source}")
    if not args.binary.is_file():
        raise SystemExit(f"missing binary {args.binary}")
    work = pathlib.Path(f"/tmp/lux-media-lane-ab-{args.throttle}")
    if work.exists():
        shutil.rmtree(work)
    config = work / "config"
    shutil.copytree(source, config)
    for suffix in ("-wal", "-shm"):
        stale = config / f"lux.db{suffix}"
        if stale.exists():
            stale.unlink()
    database = config / "lux.db"

    port = free_port()
    environment = dict(os.environ)
    environment.update(
        {
            "LUX_CONFIG_DIR": str(config),
            "LUX_READ_MODEL_MODE": "off",
            "LUX_BACKGROUND_FOREGROUND_THROTTLE": args.throttle,
            "LUX_BACKGROUND_FOREGROUND_P95_MS": str(args.threshold_ms),
            "LUX_BACKGROUND_CPU_BUDGET": str(args.budget),
            "LUX_DISCOVERY_BIND_ADDR": "127.0.0.1:0",
            "LUX_HTTP_ADDR": f"127.0.0.1:{port}",
            # The feedback logs under lux::scan_performance, which the default filter drops.
            "RUST_LOG": "luxd=info,lux=info",
        }
    )
    log_path = work / "server.log"
    log = log_path.open("ab")
    process = subprocess.Popen(
        [str(args.binary)], cwd=str(ROOT), env=environment, stdout=log, stderr=log
    )
    base = f"http://127.0.0.1:{port}"
    try:
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"{base}/health/ready", timeout=2) as response:
                    if response.status == 200:
                        break
            except Exception:
                pass
            time.sleep(0.25)
        else:
            raise SystemExit("server did not become ready")

        library_id = seed_unscanned_library(database, work / "scan-media", args.files)

        for _ in range(4):
            fetch(base, "/api/v1/home")
        idle = [fetch(base, "/api/v1/home")[1] for _ in range(20)]

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
        started = time.monotonic()
        with urllib.request.urlopen(request, timeout=120) as response:
            response.read()

        during = []
        scanned = 0
        while len(during) < args.requests and time.monotonic() - started < 180:
            _, elapsed = fetch(base, "/api/v1/home")
            during.append(elapsed)
            scanned = scan_item_count(database, library_id)

        text = log_path.read_text(errors="replace")
        print(
            "arm={arm} threshold={threshold}ms budget={budget} files={files}".format(
                arm=args.throttle,
                threshold=args.threshold_ms,
                budget=args.budget,
                files=args.files,
            )
        )
        print(
            "idle  p50={idle50:.1f} p95={idlep95:.1f} n={count}".format(
                idle50=median(idle), idlep95=percentile95(idle), count=len(idle)
            )
        )
        print(
            "scan  p50={scan50:.1f} p95={scanp95:.1f} n={count} items={items}".format(
                scan50=median(during),
                scanp95=percentile95(during),
                count=len(during),
                items=scanned,
            )
        )
        print(
            "ratio p95={ratio:.2f} (scan / idle)".format(
                ratio=percentile95(during) / max(percentile95(idle), 0.001)
            )
        )
        print(
            "logs  configured={configured} withheld={withheld} restored={restored}".format(
                configured=text.count("enabled foreground latency feedback"),
                withheld=text.count("withheld background media permits"),
                restored=text.count("restored background media permits"),
            )
        )
        return 0
    finally:
        process.terminate()
        try:
            process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            process.kill()
        log.close()
        if not args.keep_work_dir:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
