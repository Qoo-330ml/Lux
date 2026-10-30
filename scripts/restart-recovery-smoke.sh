#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/wait-for-restart.sh
source "$ROOT_DIR/scripts/lib/wait-for-restart.sh"
BIN="${LUX_BIN:-$ROOT_DIR/target/debug/luxd}"
PORT="${LUX_RECOVERY_PORT:-18607}"
FILE_COUNT="${LUX_RECOVERY_FILE_COUNT:-5000}"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lux-restart-recovery.XXXXXX")"
CONFIG_DIR="$WORK_DIR/config"
MEDIA_DIR="$WORK_DIR/media"
LOG_ONE="$WORK_DIR/first.log"
LOG_TWO="$WORK_DIR/second.log"
COOKIE_JAR="$WORK_DIR/cookies.txt"
PID=""

cleanup() {
    if [[ -n "$PID" ]] && kill -0 "$PID" 2>/dev/null; then
        kill "$PID" 2>/dev/null || true
        wait "$PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

if [[ ! -x "$BIN" ]]; then
    echo "lux binary not found: $BIN" >&2
    echo "build it first with: cargo build --locked" >&2
    exit 1
fi

mkdir -p "$CONFIG_DIR" "$MEDIA_DIR"
python3 - "$MEDIA_DIR" "$FILE_COUNT" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1])
count = int(sys.argv[2])
for index in range(count):
    title = f"Recovery Movie {index:05d}"
    directory = root / title
    directory.mkdir()
    (directory / f"{title}.2020.mkv").write_bytes(b"recovery-fixture")
PY

LUX_HTTP_ADDR="127.0.0.1:$PORT" \
LUX_CONFIG_DIR="$CONFIG_DIR" \
RUST_LOG="luxd=warn" \
"$BIN" >"$LOG_ONE" 2>&1 &
PID=$!

until curl -fsS "http://127.0.0.1:$PORT/health/live" >/dev/null; do
    if ! kill -0 "$PID" 2>/dev/null; then
        cat "$LOG_ONE" >&2
        exit 1
    fi
    sleep 0.05
done

# A fresh config directory starts in the database-selection mode, so pick SQLite before setup.
lux_select_sqlite_database "http://127.0.0.1:$PORT"

setup_payload="$(jq -n --arg root "$MEDIA_DIR" '{
    username: "admin",
    displayName: "Recovery Admin",
    password: "recovery-test-password",
    firstLibrary: { name: "Recovery Movies", kind: "MOVIE", rootPath: $root }
}')"
curl -fsS -X POST "http://127.0.0.1:$PORT/api/v1/setup/complete" \
    -H 'Content-Type: application/json' \
    -d "$setup_payload" >/dev/null

# The instance replaces itself a moment after that `201`, and the restart cancels the first scan
# that setup started and queues it again. Waiting for that window and for the recovered scan keeps
# the scan we start below from racing a dying process or answering `409` for an active scan.
lux_wait_for_restarted_server "http://127.0.0.1:$PORT"

curl -fsS -c "$COOKIE_JAR" -X POST "http://127.0.0.1:$PORT/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"recovery-test-password"}' >/dev/null

library_id="$(curl -fsS -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/api/v1/admin/libraries" | jq -r '.libraries[0].id')"
# The restart cancelled the first scan of setup and queued it again; the recovered scan is the one
# this drill interrupts on purpose, so its job id is looked up now instead of taken from the `201`.
interrupted_job_id="$(lux_wait_for_active_scan_job "http://127.0.0.1:$PORT" "$library_id" -b "$COOKIE_JAR")"

observed_processed=""
observed_total=""
for _ in $(seq 1 1200); do
    job="$(curl -fsS -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/api/v1/admin/jobs/$interrupted_job_id")"
    status="$(jq -r '.job.status' <<<"$job")"
    processed="$(jq -r '.job.processedCount' <<<"$job")"
    total="$(jq -r '.job.totalCount' <<<"$job")"
    if [[ "$status" == "RUNNING" && "$processed" -gt 0 && "$total" -gt 0 ]]; then
        observed_processed="$processed"
        observed_total="$total"
        break
    fi
    if [[ "$status" == "COMPLETED" || "$status" == "FAILED" ]]; then
        echo "scan finished before the forced termination (status=$status)" >&2
        exit 1
    fi
    sleep 0.05
done

if [[ -z "$observed_processed" ]]; then
    echo "did not observe a running scan with committed discovery progress" >&2
    exit 1
fi

kill -9 "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
PID=""

LUX_HTTP_ADDR="127.0.0.1:$PORT" \
LUX_CONFIG_DIR="$CONFIG_DIR" \
RUST_LOG="luxd=warn" \
"$BIN" >"$LOG_TWO" 2>&1 &
PID=$!

until curl -fsS "http://127.0.0.1:$PORT/health/ready" >/dev/null; do
    if ! kill -0 "$PID" 2>/dev/null; then
        cat "$LOG_TWO" >&2
        exit 1
    fi
    sleep 0.05
done

# A hard kill leaves the job RUNNING in the database. Startup cancels it with `SERVER_SHUTDOWN` and
# the first-scan recovery queues a replacement, because the library was never scanned. This is the
# contract the drill now proves: cancel, re-queue, then a successful scan that records `lastScanAt`.
interrupted_status=""
replacement_job_id=""
replacement_status=""
for _ in $(seq 1 2400); do
    jobs="$(curl -fsS -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/api/v1/admin/jobs?page=1&pageSize=100")"
    interrupted_status="$(jq -r --arg id "$interrupted_job_id" \
        '.jobs[] | select(.id == $id) | .status' <<<"$jobs")"
    replacement_job_id="$(jq -r --arg id "$interrupted_job_id" --arg library "$library_id" \
        '[.jobs[] | select(.libraryId == $library and .id != $id)]
         | sort_by([.createdAt, .id]) | last | .id // ""' <<<"$jobs")"
    if [[ -n "$replacement_job_id" && "$replacement_job_id" != "null" ]]; then
        replacement_status="$(jq -r --arg id "$replacement_job_id" \
            '.jobs[] | select(.id == $id) | .status' <<<"$jobs")"
        if [[ "$replacement_status" == "COMPLETED" ]]; then
            break
        fi
        if [[ "$replacement_status" == "FAILED" || "$replacement_status" == "CANCELLED" ]]; then
            echo "replacement scan ended in unexpected status: $replacement_status" >&2
            cat "$LOG_TWO" >&2
            exit 1
        fi
    fi
    sleep 0.1
done

if [[ "$interrupted_status" != "CANCELLED" ]]; then
    echo "the interrupted scan was not cancelled on startup: status=$interrupted_status" >&2
    cat "$LOG_TWO" >&2
    exit 1
fi
if [[ -z "$replacement_job_id" || "$replacement_job_id" == "null" ]]; then
    echo "the first-scan recovery did not queue a replacement scan" >&2
    cat "$LOG_TWO" >&2
    exit 1
fi
if [[ "$replacement_status" != "COMPLETED" ]]; then
    echo "replacement scan did not complete after the restart: status=$replacement_status" >&2
    cat "$LOG_TWO" >&2
    exit 1
fi

last_scan_at="$(curl -fsS -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/api/v1/admin/libraries" \
    | jq -r --arg id "$library_id" '.libraries[] | select(.id == $id) | .lastScanAt')"
if [[ -z "$last_scan_at" || "$last_scan_at" == "null" ]]; then
    echo "the replacement scan did not record lastScanAt" >&2
    cat "$LOG_TWO" >&2
    exit 1
fi

jq -n \
    --arg architecture "$(uname -m)" \
    --arg libraryId "$library_id" \
    --arg interruptedJobId "$interrupted_job_id" \
    --argjson processedBeforeKill "$observed_processed" \
    --argjson totalBeforeKill "$observed_total" \
    --arg interruptedStatus "$interrupted_status" \
    --arg replacementJobId "$replacement_job_id" \
    --arg replacementStatus "$replacement_status" \
    --arg lastScanAt "$last_scan_at" \
    '{architecture: $architecture, libraryId: $libraryId,
      interruptedJobId: $interruptedJobId,
      processedBeforeKill: $processedBeforeKill, totalBeforeKill: $totalBeforeKill,
      interruptedStatus: $interruptedStatus, replacementJobId: $replacementJobId,
      replacementStatus: $replacementStatus, lastScanAtRecorded: ($lastScanAt != "null")}'
