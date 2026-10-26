#!/usr/bin/env python3
"""Record endpoint latency, payload size and CPU baselines against a fixture-backed server.

The script builds the specification-scale SQLite fixture (10k movies / 1k series / 50k episodes by
default) into ``<work>/config/lux.db``, starts ``luxd`` on a loopback port, authenticates with the
fixture session cookie, and measures the read endpoints used by the first release clients:

* ``/api/v1/home``
* ``/api/v1/libraries/{movieLibrary}/items`` (``count=false`` and ``count=true``)
* ``/api/v1/search``
* ``/api/v1/items/{movie}``
* ``/api/v1/continue-watching``

Numbers are printed as a Markdown table and can be written to JSON. Results only describe the
machine and fixture recorded in the output; they are not NAS/x86_64 conclusions.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import math
import os
import pathlib
import shutil
import socket
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIXTURE_PATH = ROOT / "tools" / "catalog-fixture" / "generate.py"
COOKIE_NAME = "lux_session"


def load_fixture():
    spec = importlib.util.spec_from_file_location("lux_catalog_fixture", FIXTURE_PATH)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load fixture generator: {FIXTURE_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_ready(base_url: str, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"{base_url}/health/ready", timeout=2) as response:
                if response.status == 200:
                    return
        except (urllib.error.URLError, TimeoutError, ConnectionError) as error:
            last_error = error
        time.sleep(0.25)
    raise SystemExit(f"server did not become ready in {timeout:.0f}s: {last_error}")


def start_server(binary: pathlib.Path, config_dir: pathlib.Path, port: int, log_path: pathlib.Path):
    environment = dict(os.environ)
    environment["LUX_CONFIG_DIR"] = str(config_dir)
    environment["LUX_HTTP_ADDR"] = f"127.0.0.1:{port}"
    environment["LUX_DISCOVERY_BIND_ADDR"] = "127.0.0.1:0"
    log = log_path.open("ab")
    process = subprocess.Popen(
        [str(binary)],
        cwd=str(ROOT),
        env=environment,
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    return process, log


def stop_server(process, log) -> None:
    process.terminate()
    try:
        process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=20)
    log.close()


def bootstrap_database(binary: pathlib.Path, config_dir: pathlib.Path, log_path: pathlib.Path,
                       startup_timeout: float) -> None:
    """Start once so sqlx creates the schema, then stop before seeding rows."""

    port = free_port()
    process, log = start_server(binary, config_dir, port, log_path)
    try:
        wait_ready(f"http://127.0.0.1:{port}", startup_timeout)
    finally:
        stop_server(process, log)


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    rank = max(1, min(len(ordered), math.ceil(fraction * len(ordered))))
    return ordered[rank - 1]


def fetch(base_url: str, path: str, cookie: str, timeout: float) -> tuple[int, int, float]:
    request = urllib.request.Request(
        f"{base_url}{path}",
        headers={"Cookie": f"{COOKIE_NAME}={cookie}", "Accept": "application/json"},
    )
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=timeout) as response:
        body = response.read()
        elapsed = time.perf_counter() - started
        return response.status, len(body) if body else 0, elapsed * 1000.0


def process_cpu_seconds(pid: int) -> float | None:
    try:
        output = subprocess.run(
            ["ps", "-p", str(pid), "-o", "time="],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None
    if not output:
        return None
    parts = output.split(":")
    try:
        seconds = 0.0
        for part in parts:
            seconds = seconds * 60 + float(part)
        return seconds
    except ValueError:
        return None


def run_endpoint(
    base_url: str,
    cookie: str,
    path: str,
    *,
    warmup: int,
    requests: int,
    timeout: float,
) -> dict[str, object]:
    for _ in range(warmup):
        last_error: Exception | None = None
        for attempt in range(3):
            try:
                status, _, _ = fetch(base_url, path, cookie, timeout)
                last_error = None if status == 200 else RuntimeError(f"HTTP {status}")
            except Exception as error:
                last_error = error
            if last_error is None:
                break
            time.sleep(2)
        if last_error is not None:
            raise SystemExit(f"warmup failed for {path}: {last_error!r}")
    samples: list[float] = []
    sizes: list[int] = []
    statuses: set[int] = set()
    for _ in range(requests):
        status, size, elapsed = fetch(base_url, path, cookie, timeout)
        statuses.add(status)
        samples.append(elapsed)
        sizes.append(size)
    return {
        "path": path,
        "requests": requests,
        "statuses": sorted(statuses),
        "p50Ms": round(percentile(samples, 0.50), 2),
        "p95Ms": round(percentile(samples, 0.95), 2),
        "p99Ms": round(percentile(samples, 0.99), 2),
        "maxMs": round(max(samples), 2),
        "meanMs": round(statistics.fmean(samples), 2),
        "meanBytes": int(statistics.fmean(sizes)) if sizes else 0,
        "totalBytes": sum(sizes),
    }


def size_of(path: pathlib.Path) -> int:
    try:
        return path.stat().st_size
    except OSError:
        return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary",
        type=pathlib.Path,
        default=ROOT / "target" / "release" / "luxd",
        help="luxd binary to run (release builds are required for recorded baselines)",
    )
    parser.add_argument("--work-dir", type=pathlib.Path, help="reuse/create this directory")
    parser.add_argument("--keep-work-dir", action="store_true")
    parser.add_argument("--requests", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--startup-timeout", type=float, default=120.0)
    parser.add_argument(
        "--cold-timeout",
        type=float,
        default=180.0,
        help="timeout for the first home request after startup (cold cache); 0 skips it",
    )
    parser.add_argument("--movies", type=int, default=10_000)
    parser.add_argument("--series", dest="series_count", type=int, default=1_000)
    parser.add_argument("--episodes-per-series", type=int, default=50)
    parser.add_argument("--seasons-per-series", type=int, default=5)
    parser.add_argument("--detail-items", type=int, default=2_000)
    parser.add_argument("--user-states", type=int, default=2_000)
    parser.add_argument("--posters", type=int, default=20)
    parser.add_argument("--json-out", type=pathlib.Path)
    parser.add_argument(
        "--label",
        help="optional record label printed with the table (e.g. commit and profile)",
    )
    args = parser.parse_args()

    if not args.binary.is_file():
        raise SystemExit(
            f"luxd binary not found: {args.binary}; run `cargo build --release --locked` first"
        )

    work_dir = args.work_dir or pathlib.Path(tempfile.mkdtemp(prefix="lux-api-perf-"))
    work_dir = work_dir.expanduser().resolve()
    config_dir = work_dir / "config"
    config_dir.mkdir(parents=True, exist_ok=True)
    database = config_dir / "lux.db"
    if database.exists():
        database.unlink()
    for suffix in ("-wal", "-shm"):
        stale = pathlib.Path(f"{database}{suffix}")
        if stale.exists():
            stale.unlink()

    fixture = load_fixture()
    bootstrap_database(args.binary, config_dir, work_dir / "bootstrap.log", args.startup_timeout)
    seed_started = time.perf_counter()
    token = "lux-api-performance-token"
    manifest = fixture.seed_spec_target(
        database,
        work_dir / "media",
        movies=args.movies,
        series_count=args.series_count,
        episodes_per_series=args.episodes_per_series,
        seasons_per_series=args.seasons_per_series,
        detail_items=args.detail_items,
        user_states=args.user_states,
        poster_limit=args.posters,
        session_token=token,
        now=int(time.time()),
    )
    seed_seconds = time.perf_counter() - seed_started

    # The fixture references media paths that do not exist on disk. Marking the per-root local
    # metadata backfill completed keeps that worker from continuously taking SQLite's write lock
    # (which otherwise can starve the first home request's recommendation-stat refresh).
    connection = sqlite3.connect(database)
    try:
        connection.execute(
            "INSERT INTO scan_local_metadata_backfills(library_root_id, status)"
            " SELECT id, 'COMPLETED' FROM library_roots WHERE true"
            " ON CONFLICT(library_root_id) DO UPDATE SET status = 'COMPLETED'"
        )
        connection.commit()
    finally:
        connection.close()

    port = free_port()
    base_url = f"http://127.0.0.1:{port}"
    server, server_log = start_server(args.binary, config_dir, port, work_dir / "server.log")
    try:
        wait_ready(base_url, args.startup_timeout)
        cold_home: dict[str, object] | None = None
        if args.cold_timeout > 0:
            started = time.perf_counter()
            try:
                status, size, _ = fetch(base_url, "/api/v1/home", token, args.cold_timeout)
                cold_home = {
                    "status": status,
                    "bytes": size,
                    "elapsedMs": round((time.perf_counter() - started) * 1000.0, 1),
                }
            except Exception as error:
                cold_home = {
                    "status": None,
                    "timeout": True,
                    "elapsedMs": round((time.perf_counter() - started) * 1000.0, 1),
                    "error": repr(error),
                }
        movie_library = next(
            library["id"] for library in manifest["libraries"] if library["kind"] == "MOVIE"
        )
        movie_id = manifest["sampleMovieId"]
        endpoints = [
            ("home", "/api/v1/home"),
            (
                "library-items-count-off",
                f"/api/v1/libraries/{movie_library}/items?pageSize=24&count=false",
            ),
            (
                "library-items-count-on",
                f"/api/v1/libraries/{movie_library}/items?pageSize=24&count=true",
            ),
            ("search", "/api/v1/search?q=Spec&pageSize=24"),
            ("detail", f"/api/v1/items/{movie_id}"),
            ("continue-watching", "/api/v1/continue-watching?pageSize=24"),
        ]
        database_bytes = size_of(database)
        wal_bytes = size_of(pathlib.Path(f"{database}-wal"))
        results = []
        cpu_before = process_cpu_seconds(server.pid)
        for label, path in endpoints:
            try:
                result = run_endpoint(
                    base_url,
                    token,
                    path,
                    warmup=args.warmup,
                    requests=args.requests,
                    timeout=args.timeout,
                )
            except Exception as error:  # surface the endpoint that failed, then stop
                raise SystemExit(f"{label} ({path}) failed: {error!r}") from error
            results.append({"label": label} | result)
        cpu_after = process_cpu_seconds(server.pid)
    finally:
        stop_server(server, server_log)
        if not args.keep_work_dir and args.work_dir is None:
            shutil.rmtree(work_dir, ignore_errors=True)

    total_requests = sum(result["requests"] for result in results)
    cpu_ms = None
    if cpu_before is not None and cpu_after is not None:
        cpu_ms = round((cpu_after - cpu_before) * 1000.0 / max(1, total_requests), 3)

    summary = {
        "label": args.label,
        "architecture": os.uname().machine,
        "fixture": {key: manifest[key] for key in manifest if key.endswith("Count")}
        | {"contentSha256": manifest["contentSha256"]},
        "seedSeconds": round(seed_seconds, 2),
        "databaseBytes": database_bytes,
        "walBytes": wal_bytes,
        "cpuMsPerRequest": cpu_ms,
        "coldHome": cold_home,
        "endpoints": results,
    }

    print()
    if args.label:
        print(f"Fixture label: {args.label}")
    print(
        "Fixture: "
        + ", ".join(f"{key}={value}" for key, value in summary["fixture"].items())
    )
    print(
        f"Database {database_bytes} B, WAL {wal_bytes} B, seed {seed_seconds:.2f}s, "
        f"CPU {cpu_ms} ms/request, arch {summary['architecture']}"
    )
    if cold_home is not None:
        print(f"Cold home: {cold_home}")
    print()
    print("| endpoint | p50 ms | p95 ms | p99 ms | mean bytes | requests | status |")
    print("|---|---:|---:|---:|---:|---:|---|")
    for result in results:
        print(
            f"| {result['label']} | {result['p50Ms']} | {result['p95Ms']} | "
            f"{result['p99Ms']} | {result['meanBytes']} | {result['requests']} | "
            f"{result['statuses']} |"
        )
    if any(result["statuses"] != [200] for result in results):
        print("FAILED: some endpoints did not return 200", file=sys.stderr)
        return 1
    if args.json_out is not None:
        args.json_out.write_text(
            json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
