#!/usr/bin/env python3
"""Probe whether holding back the background media lane protects the foreground page.

`scripts/compare-read-model-modes.py` cannot answer that: it seeds scan libraries during its prepare
phase, so the measurement-time "re-scan" only re-walks directories and the media lane (probe, NFO,
image, thumbnail) has no work to hold back. This probe seeds a library with files the scanner has
never seen, measures `/api/v1/home` while that library is actually being scanned, and prints the
"scan / idle" p95 ratio. Run it once per arm and compare:

    python3 scripts/probe-media-lane-ab.py --throttle off --work-dir /tmp/lux-rm-resume3 \
        --fake-media-cpu-ms 150
    python3 scripts/probe-media-lane-ab.py --throttle on --work-dir /tmp/lux-rm-resume3 \
        --fake-media-cpu-ms 150

`--fake-media-cpu-ms` installs synthetic ffprobe/ffmpeg that burn that much CPU per call. It matters:
on a host without ffprobe/ffmpeg the fixture's probes fail instantly, the media lane holds a permit
for microseconds, and the downgrade under test has nothing to hold back (measured: identical ratios).

`--threshold-ms 0` derives the threshold from a pre-measured idle p50 (x2). `--release-check` waits
out the 30 s sample window and rescans a fresh library to force a media-lane acquisition, but it
needs a quiet host: on a loaded machine (load average ~4 here) the idle p95 drifted 117-261 ms, so a
threshold that engages during the scan can also stay above the idle page and suppress the release.

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


FAKE_FFPROBE = """#!/bin/sh
# Synthetic ffprobe: burns CPU for {cpu_ms} ms, then prints a probe result. Stands in for a real
# ffprobe so the media lane actually occupies CPU while the scan runs.
python3 - "$@" <<'PY'
import time
end = time.perf_counter() + {seconds}
while time.perf_counter() < end:
    pass
print('{{"format":{{"format_name":"matroska","duration":"12.5","bit_rate":"500000"}},"streams":[{{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080}}]}}')
PY
"""

FAKE_FFMPEG = """#!/bin/sh
# Synthetic ffmpeg: burns CPU briefly and writes a tiny jpeg to the last .jpg argument.
python3 - "$@" <<'PY'
import sys, time
end = time.perf_counter() + {seconds}
while time.perf_counter() < end:
    pass
targets = [argument for argument in sys.argv[1:] if argument.endswith(".jpg")]
if targets:
    with open(targets[-1], "wb") as handle:
        handle.write(b"\\xff\\xd8lux-thumb\\xff\\xd9")
PY
"""


def synthetic_media_tools(work: pathlib.Path, cpu_ms: int) -> pathlib.Path:
    """Write fake ffprobe/ffmpeg into the work dir so the media lane does real (synthetic) CPU work.

    Without them the fixture's probes and thumbnails fail instantly, the media lane never holds a
    permit for long, and a downgrade test measures nothing. Every new executable is warmed once
    because a first execution on this kind of host can be delayed by host-side scanning.
    """
    directory = work / "media-bin"
    directory.mkdir(parents=True, exist_ok=True)
    tools = {
        "ffprobe": FAKE_FFPROBE.format(cpu_ms=cpu_ms, seconds=cpu_ms / 1000.0),
        "ffmpeg": FAKE_FFMPEG.format(seconds=max(cpu_ms, 20) / 3000.0),
    }
    for name, body in tools.items():
        path = directory / name
        path.write_text(body)
        path.chmod(0o755)
    for _ in range(2):
        subprocess.run([str(directory / "ffprobe"), "-version"], check=False,
                       capture_output=True)
    return directory


def wait_ready(base: str, timeout: float = 240.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"{base}/health/ready", timeout=2) as response:
                if response.status == 200:
                    return
        except Exception:
            pass
        time.sleep(0.25)
    raise SystemExit("server did not become ready")


def start_server(
    binary: pathlib.Path,
    config: pathlib.Path,
    media_bin: pathlib.Path | None,
    throttle: str,
    threshold_ms: int,
    budget: int,
    log_path: pathlib.Path,
):
    port = free_port()
    environment = dict(os.environ)
    if media_bin is not None:
        environment["PATH"] = f"{media_bin}{os.pathsep}{environment.get('PATH', '')}"
    environment.update(
        {
            "LUX_CONFIG_DIR": str(config),
            "LUX_READ_MODEL_MODE": "off",
            "LUX_BACKGROUND_FOREGROUND_THROTTLE": throttle,
            "LUX_BACKGROUND_FOREGROUND_P95_MS": str(threshold_ms),
            "LUX_BACKGROUND_CPU_BUDGET": str(budget),
            "LUX_DISCOVERY_BIND_ADDR": "127.0.0.1:0",
            "LUX_HTTP_ADDR": f"127.0.0.1:{port}",
            # The feedback logs under lux::scan_performance, which the default filter drops.
            "RUST_LOG": "luxd=info,lux=info",
        }
    )
    log = log_path.open("ab")
    process = subprocess.Popen(
        [str(binary)], cwd=str(ROOT), env=environment, stdout=log, stderr=log
    )
    base = f"http://127.0.0.1:{port}"
    try:
        wait_ready(base)
    except BaseException:
        stop_server(process, log)
        raise
    return process, log, base


def stop_server(process, log) -> None:
    process.terminate()
    try:
        process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        process.kill()
    log.close()


def seed_unscanned_library(
    database: pathlib.Path, media_root: pathlib.Path, files: int, name: str = "library-0"
) -> str:
    """Create a movie library with real files and no catalog rows, so the scan has work to do."""
    root = media_root / name
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
    parser.add_argument(
        "--threshold-ms",
        type=int,
        default=100,
        help="0 derives it from a pre-measured idle p50 (x2); p95 is too noisy on a loaded host",
    )
    parser.add_argument("--files", type=int, default=1200)
    parser.add_argument("--requests", type=int, default=30)
    parser.add_argument("--budget", type=int, default=9)
    parser.add_argument(
        "--release-check",
        action="store_true",
        help="after the scan, wait out the sample window and prove the permits come back",
    )
    parser.add_argument(
        "--fake-media-cpu-ms",
        type=int,
        default=0,
        help="install synthetic ffprobe/ffmpeg that burn this much CPU per call (0 uses PATH)",
    )
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

    media_bin = None
    if args.fake_media_cpu_ms > 0:
        media_bin = synthetic_media_tools(work, args.fake_media_cpu_ms)

    threshold_ms = args.threshold_ms
    if threshold_ms == 0:
        # A fixed millisecond threshold is fragile on a loaded host: idle p95 drifted 117-250 ms
        # between runs here. Measure the idle page once with the feedback disabled, then target
        # comfortably above it.
        baseline_process, baseline_log, baseline_base = start_server(
            args.binary, config, media_bin, "off", 300, args.budget, work / "baseline.log"
        )
        try:
            for _ in range(4):
                fetch(baseline_base, "/api/v1/home")
            baseline = [fetch(baseline_base, "/api/v1/home")[1] for _ in range(20)]
        finally:
            stop_server(baseline_process, baseline_log)
        # p50 is stable across runs here (112-140 ms) while p95 swings with host load, so derive
        # from the median and leave headroom for the scan spike.
        threshold_ms = max(150, int(median(baseline) * 2))
        print(
            "idle baseline p50={p50:.1f} p95={p95:.1f} ms -> threshold {threshold} ms".format(
                p50=median(baseline), p95=percentile95(baseline), threshold=threshold_ms
            )
        )

    log_path = work / "server.log"
    process, log, base = start_server(
        args.binary, config, media_bin, args.throttle, threshold_ms, args.budget, log_path
    )
    try:
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

        release_seconds = None
        if args.release_check:
            # The feedback only sees samples from its window, so let it move past the scan while
            # issuing idle requests, then force a media-lane acquisition with a second scan.
            waited = 0.0
            while waited < 40.0:
                fetch(base, "/api/v1/home")
                time.sleep(1.0)
                waited += 1.0 + 0.2
            # Re-walking an unchanged library does no media-lane work, so the feedback would never
            # re-evaluate. A fresh library gives the second scan real probe work.
            recovery_library = seed_unscanned_library(
                database, work / "scan-media", 20, "library-recovery"
            )
            request = urllib.request.Request(
                f"{base}/api/v1/admin/libraries/{recovery_library}/scan",
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
            while time.monotonic() - started < 60:
                if "restored background media permits" in log_path.read_text(
                    errors="replace"
                ):
                    release_seconds = time.monotonic() - started
                    break
                time.sleep(0.5)

        text = log_path.read_text(errors="replace")
        print(
            "arm={arm} threshold={threshold}ms budget={budget} files={files} "
            "media_cpu={media}ms".format(
                arm=args.throttle,
                threshold=threshold_ms,
                budget=args.budget,
                files=args.files,
                media=args.fake_media_cpu_ms,
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
        if args.release_check:
            print(
                "release after recovery: {value}".format(
                    value=(
                        f"{release_seconds:.1f}s"
                        if release_seconds is not None
                        else "NOT OBSERVED"
                    )
                )
            )
        return 0
    finally:
        stop_server(process, log)
        if not args.keep_work_dir:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
