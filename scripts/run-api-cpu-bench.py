#!/usr/bin/env python3
"""CPU per request for the read-heavy endpoints (W5 baseline and A/B).

Samples the server process' cumulative CPU time (user + system) around a fixed number of sequential
requests and reports CPU per request per endpoint. Wall time is reported next to it so a CPU win is
not confused with a latency win.

    python3 scripts/run-api-cpu-bench.py --work-dir /tmp/lux-rm-resume3 --requests 200

Only stdlib is used. Numbers describe this machine and fixture; they are not NAS/x86_64 results.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
COOKIE = "lux_session"
TOKEN = "lux-api-performance-token"

ENDPOINTS = (
    ("home", "/api/v1/home"),
    ("list-50", "/api/v1/libraries/{library}/items?pageSize=50"),
    ("search", "/api/v1/search?q=Spec%20Movie"),
    ("continue-watching", "/api/v1/continue-watching?pageSize=24"),
    ("libraries", "/api/v1/libraries?pageSize=100"),
)


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def fetch(base: str, path: str, timeout: float) -> tuple[bytes, float]:
    request = urllib.request.Request(f"{base}{path}", headers={"Cookie": f"{COOKIE}={TOKEN}"})
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=timeout) as response:
        body = response.read()
    return body, (time.perf_counter() - started) * 1000.0


def process_cpu_seconds(pid: int) -> float:
    """Cumulative user+system CPU time of `pid` in seconds."""
    output = subprocess.run(
        ["ps", "-o", "time=", "-p", str(pid)],
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    if not output:
        raise SystemExit(f"process {pid} is gone")
    days = 0
    if "-" in output:
        day_part, output = output.split("-", 1)
        days = int(day_part)
    parts = output.split(":")
    seconds = float(parts[-1])
    if len(parts) >= 2:
        seconds += int(parts[-2]) * 60
    if len(parts) >= 3:
        seconds += int(parts[-3]) * 3600
    return days * 86_400 + seconds


class Server:
    def __init__(self, binary: pathlib.Path, config: pathlib.Path, log: pathlib.Path, mode: str):
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
        raise SystemExit("server did not become ready")

    def close(self) -> None:
        self.process.terminate()
        try:
            self.process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            self.process.kill()
        self.log.close()


def first_library_id(database: pathlib.Path) -> str:
    import sqlite3

    # A WAL database cannot be opened read-only from another process without its -shm file; this is
    # our own copy, so open it normally.
    connection = sqlite3.connect(database, timeout=5)
    try:
        row = connection.execute(
            "SELECT id FROM libraries WHERE is_enabled = 1 AND system_kind IS NULL"
            " ORDER BY name LIMIT 1"
        ).fetchone()
    finally:
        connection.close()
    if row is None:
        raise SystemExit("fixture has no enabled library")
    return row[0]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path, default=ROOT / "target/release/luxd")
    parser.add_argument("--work-dir", type=pathlib.Path, required=True)
    parser.add_argument("--mode", default="off", choices=("off", "on", "shadow"))
    parser.add_argument("--requests", type=int, default=200)
    parser.add_argument("--warmup", type=int, default=20)
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--label", default="")
    parser.add_argument("--json-out", type=pathlib.Path)
    args = parser.parse_args()

    if not args.binary.is_file():
        raise SystemExit(f"luxd binary not found: {args.binary}")
    source = args.work_dir.resolve() / "config"
    if not (source / "lux.db").is_file():
        raise SystemExit(f"fixture database not found under {source}")
    work = pathlib.Path(f"/tmp/lux-cpu-bench-{os.getpid()}")
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
    library_id = first_library_id(database)

    server = Server(args.binary, config, work / "server.log", args.mode)
    try:
        server.wait_ready(args.timeout)
        time.sleep(1.0)
        results = {}
        for label, template in ENDPOINTS:
            path = template.format(library=library_id)
            for _ in range(args.warmup):
                fetch(server.base, path, args.timeout)
            cpu_before = process_cpu_seconds(server.process.pid)
            wall_started = time.perf_counter()
            for _ in range(args.requests):
                fetch(server.base, path, args.timeout)
            wall_seconds = time.perf_counter() - wall_started
            cpu_seconds = process_cpu_seconds(server.process.pid) - cpu_before
            results[label] = {
                "requests": args.requests,
                "cpu_ms_per_request": round(cpu_seconds * 1000 / args.requests, 3),
                "wall_ms_per_request": round(wall_seconds * 1000 / args.requests, 2),
            }
    finally:
        server.close()

    report = {
        "label": args.label,
        "mode": args.mode,
        "requests": args.requests,
        "endpoints": results,
    }
    print(json.dumps(report, indent=2))
    if args.json_out:
        args.json_out.write_text(json.dumps(report, indent=2))
    shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
