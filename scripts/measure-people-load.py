#!/usr/bin/env python3
"""Evidence probe: what does loading item actors cost on the Lux detail path?

Actor relations are file-backed, and every Lux detail request (and every Emby list page with
`Fields=People`) reads one `people.json` per item plus a profile-image lookup per actor. Whether a
page-level cache or a materialized batch is worth writing depends on that cost, so this probe
measures the same detail requests twice: once with no relations (the 66k fixture carries none) and
once after generating `people.json` for the measured items.

    python3 scripts/measure-people-load.py --work-dir /tmp/lux-rm-resume3
    python3 scripts/measure-people-load.py --work-dir /tmp/lux-rm-resume3 --actors 25 --rounds 6

Only stdlib is used. Numbers describe this machine and this fixture (read model off, 66k catalog,
warm local disk); run it on the storage you actually care about before deciding anything.
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
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
COOKIE = "lux_session"
TOKEN = "lux-api-performance-token"
WORK = pathlib.Path("/tmp/lux-people-load")


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


class Server:
    def __init__(self, binary: pathlib.Path, config: pathlib.Path):
        self.port = free_port()
        environment = dict(os.environ)
        environment.update(
            {
                "LUX_CONFIG_DIR": str(config),
                "LUX_READ_MODEL_MODE": "off",
                "LUX_DISCOVERY_BIND_ADDR": "127.0.0.1:0",
                "LUX_HTTP_ADDR": f"127.0.0.1:{self.port}",
                "RUST_LOG": "luxd=warn",
            }
        )
        self.log = (WORK / f"server-{self.port}.log").open("ab")
        self.process = subprocess.Popen(
            [str(binary)], cwd=str(ROOT), env=environment, stdout=self.log, stderr=self.log
        )
        self.base = f"http://127.0.0.1:{self.port}"
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"{self.base}/health/ready", timeout=2) as response:
                    if response.status == 200:
                        return
            except Exception:
                pass
            time.sleep(0.25)
        raise SystemExit("server did not become ready")

    def close(self) -> None:
        self.process.terminate()
        try:
            self.process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self.process.kill()
        self.log.close()


def item_ids(database: pathlib.Path, count: int) -> list[str]:
    # immutable=1: the copy is static and has no -shm file yet (read-only WAL opens need one).
    connection = sqlite3.connect(f"file:{database}?immutable=1", uri=True)
    try:
        return [
            row[0]
            for row in connection.execute(
                "SELECT id FROM media_items WHERE item_type = 'MOVIE'"
                " AND removed_at IS NULL ORDER BY id LIMIT ?",
                (count,),
            )
        ]
    finally:
        connection.close()


def write_relations(config: pathlib.Path, ids: list[str], actors: int) -> None:
    """Write legacy-shaped relations: a JSON array of actors per item."""
    root = config / "metadata" / "library"
    for index, item_id in enumerate(ids):
        shard = hashlib.sha256(item_id.encode()).hexdigest()[:2]
        directory = root / shard / item_id
        directory.mkdir(parents=True, exist_ok=True)
        payload = [
            {
                "id": f"{index:02d}{actor:03d}",
                "name": f"Actor {index}-{actor}",
                "provider": "tmdb",
                "character": f"Role {actor}",
                "order": actor,
            }
            for actor in range(actors)
        ]
        (directory / "people.json").write_text(json.dumps(payload))


def measure(server: Server, ids: list[str], rounds: int) -> dict[str, float | int]:
    for item_id in ids[:5]:
        fetch(server.base, f"/api/v1/items/{item_id}")
    samples = []
    sizes = []
    for _ in range(rounds):
        for item_id in ids:
            size, elapsed = fetch(server.base, f"/api/v1/items/{item_id}")
            samples.append(elapsed)
            sizes.append(size)
    samples.sort()
    return {
        "count": len(samples),
        "p50": statistics.median(samples),
        "p95": samples[max(0, int(len(samples) * 0.95) - 1)],
        "bytes": int(statistics.median(sizes)),
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
    parser.add_argument("--actors", type=int, default=10)
    parser.add_argument("--rounds", type=int, default=4)
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
    ids = item_ids(database, args.items)
    print(f"items={len(ids)} actors_per_item={args.actors} rounds={args.rounds}")

    server = Server(args.binary, config)
    try:
        without = measure(server, ids, args.rounds)
    finally:
        server.close()

    write_relations(config, ids, args.actors)

    server = Server(args.binary, config)
    try:
        with_relations = measure(server, ids, args.rounds)
    finally:
        server.close()

    print("without relations: p50={p50:.2f} ms p95={p95:.2f} ms bytes={bytes}".format(**without))
    print(
        "with relations   : p50={p50:.2f} ms p95={p95:.2f} ms bytes={bytes}".format(
            **with_relations
        )
    )
    print(
        "delta per request: p50={p50:+.2f} ms p95={p95:+.2f} ms".format(
            p50=with_relations["p50"] - without["p50"],
            p95=with_relations["p95"] - without["p95"],
        )
    )
    if not args.keep_work_dir:
        shutil.rmtree(WORK, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
