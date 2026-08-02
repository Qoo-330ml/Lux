#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
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

setup_payload="$(jq -n --arg root "$MEDIA_DIR" '{
    username: "admin",
    displayName: "Recovery Admin",
    password: "recovery-test-password",
    firstLibrary: { name: "Recovery Movies", kind: "MOVIE", rootPath: $root }
}')"
curl -fsS -X POST "http://127.0.0.1:$PORT/api/v1/setup/complete" \
    -H 'Content-Type: application/json' \
    -d "$setup_payload" >/dev/null

curl -fsS -c "$COOKIE_JAR" -X POST "http://127.0.0.1:$PORT/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"recovery-test-password"}' >/dev/null
CSRF="$(awk '$0 !~ /^#/ && $6 == "lux_csrf" { print $7 }' "$COOKIE_JAR")"
if [[ -z "$CSRF" ]]; then
    echo "login did not return a CSRF cookie" >&2
    exit 1
fi

library_id="$(curl -fsS -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/api/v1/admin/libraries" | jq -r '.libraries[0].id')"
job_id="$(curl -fsS -b "$COOKIE_JAR" -H "X-CSRF-Token: $CSRF" \
    -X POST "http://127.0.0.1:$PORT/api/v1/admin/libraries/$library_id/scan" \
    | jq -r '.job.id')"

observed_processed=""
observed_total=""
for _ in $(seq 1 400); do
    job="$(curl -fsS -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/api/v1/admin/jobs/$job_id")"
    status="$(jq -r '.job.status' <<<"$job")"
    processed="$(jq -r '.job.processedCount' <<<"$job")"
    total="$(jq -r '.job.totalCount' <<<"$job")"
    if [[ "$status" == "RUNNING" && "$processed" -gt 0 && "$processed" -lt "$total" ]]; then
        observed_processed="$processed"
        observed_total="$total"
        break
    fi
    if [[ "$status" == "COMPLETED" || "$status" == "FAILED" ]]; then
        echo "scan completed before the forced termination (status=$status)" >&2
        exit 1
    fi
    sleep 0.05
done

if [[ -z "$observed_processed" ]]; then
    echo "did not observe a running scan after the first committed batch" >&2
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

final_status=""
final_processed=""
final_total=""
for _ in $(seq 1 600); do
    job="$(curl -fsS -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/api/v1/admin/jobs/$job_id")"
    final_status="$(jq -r '.job.status' <<<"$job")"
    final_processed="$(jq -r '.job.processedCount' <<<"$job")"
    final_total="$(jq -r '.job.totalCount' <<<"$job")"
    if [[ "$final_status" == "COMPLETED" ]]; then
        break
    fi
    if [[ "$final_status" == "FAILED" || "$final_status" == "CANCELLED" ]]; then
        echo "resumed scan ended in unexpected status: $final_status" >&2
        cat "$LOG_TWO" >&2
        exit 1
    fi
    sleep 0.1
done

if [[ "$final_status" != "COMPLETED" || "$final_processed" != "$final_total" || "$final_total" != "$observed_total" ]]; then
    echo "scan did not complete after restart: status=$final_status processed=$final_processed total=$final_total expected=$observed_total" >&2
    cat "$LOG_TWO" >&2
    exit 1
fi

jq -n \
    --arg architecture "$(uname -m)" \
    --arg jobId "$job_id" \
    --argjson processedBeforeKill "$observed_processed" \
    --argjson totalBeforeKill "$observed_total" \
    --arg finalStatus "$final_status" \
    --argjson processedAfterRestart "$final_processed" \
    --argjson totalAfterRestart "$final_total" \
    '{architecture: $architecture, jobId: $jobId, processedBeforeKill: $processedBeforeKill, totalBeforeKill: $totalBeforeKill, finalStatus: $finalStatus, processedAfterRestart: $processedAfterRestart, totalAfterRestart: $totalAfterRestart}'
