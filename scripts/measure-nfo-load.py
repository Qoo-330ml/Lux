#!/usr/bin/env python3
"""Evidence probe: cost of reading + decoding the derived NFO cache on the Lux detail path.

The derived cache lives in `media_item_nfo_cache` and is read once per item, then decoded from JSON
and merged into the detail DTO. The 66k fixture carries 200 rows of 131,042 bytes each, so this
probe compares detail requests for items *with* that payload against items without one, interleaved
so host drift cannot explain the difference.

    python3 scripts/measure-nfo-load.py --work-dir /tmp/lux-rm-resume3

Only stdlib is used. Numbers describe this machine and this fixture (read model off, 66k catalog);
the fixture's NFO payload is padded to a size limit, so treat it as an upper bound.
"""

from __future__ import annotations

import argparse
import os
import pathlib
import shutil
import socket
import sqlite3
import statistics
import subprocess
import sys
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
COOKIE = "lux_session"
TOKEN = "lux-api-performance-token"
WORK = pathlib.Path("/tmp/lux-nfo-load")


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def fetch(base: str, path: str) -> tuple[int, float]:
    request = urllib.request.Request(
        f"{base}{path}", headers={"Cookie": f"{COOKIE}={TOKEN}"}
    )
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=60) as response:
        body = response.read()
    return len(body), (time.perf_counter() - started) * 1000.0


def start(binary: pathlib.Path, config: pathlib.Path):
    port = free_port()
    environment = dict(os.environ)
    environment.update(
        {
            "LUX_CONFIG_DIR": str(config),
            "LUX_READ_MODEL_MODE": "off",
            "LUX_DISCOVERY_BIND_ADDR": "127.0.0.1:0",
            "LUX_HTTP_ADDR": f"127.0.0.1:{port}",
            "RUST_LOG": "luxd=warn",
        }
    )
    log = (WORK / f"server-{port}.log").open("ab")
    process = subprocess.Popen(
        [str(binary)], cwd=str(ROOT), env=environment, stdout=log, stderr=log
    )
    base = f"http://127.0.0.1:{port}"
    deadline = time.monotonic() + 240
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"{base}/health/ready", timeout=2) as response:
                if response.status == 200:
                    return process, log, base
        except Exception:
            pass
        time.sleep(0.25)
    raise SystemExit("server did not become ready")


def stop(process, log) -> None:
    process.terminate()
    try:
        process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        process.kill()
    log.close()


def groups(database: pathlib.Path, limit: int) -> tuple[list[str], list[str]]:
    connection = sqlite3.connect(f"file:{database}?immutable=1", uri=True)
    try:
        with_nfo = [
            row[0]
            for row in connection.execute(
                "SELECT m.id FROM media_item_nfo_cache c JOIN media_items m ON m.id = c.item_id"
                " WHERE m.item_type = 'MOVIE' AND m.removed_at IS NULL ORDER BY m.id LIMIT ?",
                (limit,),
            )
        ]
        without_nfo = [
            row[0]
            for row in connection.execute(
                "SELECT m.id FROM media_items m WHERE m.item_type = 'MOVIE'"
                " AND m.removed_at IS NULL"
                " AND NOT EXISTS (SELECT 1 FROM media_item_nfo_cache c WHERE c.item_id = m.id)"
                " ORDER BY m.id LIMIT ?",
                (limit,),
            )
        ]
    finally:
        connection.close()
    return with_nfo, without_nfo


def measure(base: str, item_ids: list[str], rounds: int) -> dict[str, float | int]:
    for item_id in item_ids[:3]:
        fetch(base, f"/api/v1/items/{item_id}")
    samples = []
    sizes = []
    for _ in range(rounds):
        for item_id in item_ids:
            size, elapsed = fetch(base, f"/api/v1/items/{item_id}")
            samples.append(elapsed)
            sizes.append(size)
    samples.sort()
    return {
        "p50": statistics.median(samples),
        "p95": samples[max(0, int(len(samples) * 0.95) - 1)],
        "bytes": int(statistics.median(sizes)),
        "n": len(samples),
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
    parser.add_argument("--items", type=int, default=50)
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--keep-work-dir", action="store_true")
    args = parser.parse_args()

    if not args.binary.is_file():
        raise SystemExit(f"missing binary {args.binary}")
    source = args.work_dir.resolve() / "config"
    if not (source / "lux.db").is_file():
        raise SystemExit(f"fixture database not found under {source}")
    if WORK.exists():
        shutil.rmtree(WORK)
    config = WORK / "config"
    shutil.copytree(source, config)
    for suffix in ("-wal", "-shm"):
        stale = config / f"lux.db{suffix}"
        if stale.exists():
            stale.unlink()
    database = config / "lux.db"
    with_nfo, without_nfo = groups(database, args.items)
    print(f"items_with_nfo={len(with_nfo)} items_without_nfo={len(without_nfo)} rounds={args.rounds}")

    process, log, base = start(args.binary, config)
    try:
        # Interleave the two groups so load drift cannot explain the difference.
        first_without = measure(base, without_nfo, args.rounds)
        first_with = measure(base, with_nfo, args.rounds)
        second_without = measure(base, without_nfo, args.rounds)
        second_with = measure(base, with_nfo, args.rounds)
    finally:
        stop(process, log)

    for label, result in (
        ("without nfo ", first_without),
        ("with nfo    ", first_with),
        ("without nfo2", second_without),
        ("with nfo2   ", second_with),
    ):
        print(
            "{label}: p50={p50:.2f} ms p95={p95:.2f} ms bytes={bytes} n={n}".format(
                label=label, **result
            )
        )
    print(
        "delta       : p50={p50:+.2f} ms p95={p95:+.2f} ms".format(
            p50=(first_with["p50"] + second_with["p50"]) / 2
            - (first_without["p50"] + second_without["p50"]) / 2,
            p95=(first_with["p95"] + second_with["p95"]) / 2
            - (first_without["p95"] + second_without["p95"]) / 2,
        )
    )
    if not args.keep_work_dir:
        shutil.rmtree(WORK, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
