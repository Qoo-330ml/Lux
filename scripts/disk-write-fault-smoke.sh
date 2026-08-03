#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

IMAGE="${LUX_IMAGE:-lux:arm64-local}"
PORT="${LUX_DISK_FAULT_PORT:-18608}"
TMPFS_SIZE="${LUX_DISK_FAULT_TMPFS_SIZE:-64m}"
NAME="${LUX_DISK_FAULT_CONTAINER:-lux-disk-write-fault}"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lux-disk-write-fault.XXXXXX")"

cleanup() {
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

for command in curl docker jq; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "required command not found: $command" >&2
        exit 1
    fi
done

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --rm \
    --name "$NAME" \
    -p "$PORT:8097" \
    --tmpfs "/data:rw,size=$TMPFS_SIZE,uid=10001,gid=10001,mode=755" \
    "$IMAGE" >"$WORK_DIR/container-id"

started=0
for _ in $(seq 1 30); do
    if curl -fsS "http://127.0.0.1:$PORT/health/live" >/dev/null 2>&1; then
        started=1
        break
    fi
    sleep 1
done
if [[ "$started" != 1 ]]; then
    docker logs "$NAME" >&2
    exit 1
fi

curl -fsS -X POST "http://127.0.0.1:$PORT/api/v1/setup/complete" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","displayName":"Disk Fault Admin","password":"disk-write-fault-test-password"}' \
    >"$WORK_DIR/setup.json"

curl -sS -D "$WORK_DIR/login.headers" -o "$WORK_DIR/login.json" \
    -X POST "http://127.0.0.1:$PORT/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"disk-write-fault-test-password"}'

session_cookie="$(grep -i '^set-cookie: lux_session=' "$WORK_DIR/login.headers" \
    | sed -E 's/^[^:]+: (lux_session=[^;]+).*/\1/I' | tail -n 1)"
csrf_cookie="$(grep -i '^set-cookie: lux_csrf=' "$WORK_DIR/login.headers" \
    | sed -E 's/^[^:]+: (lux_csrf=[^;]+).*/\1/I' | tail -n 1)"
if [[ -z "$session_cookie" || -z "$csrf_cookie" ]]; then
    echo "login did not return both session cookies" >&2
    exit 1
fi
csrf_value="${csrf_cookie#lux_csrf=}"
cookies="$session_cookie; $csrf_cookie"

docker exec --user 0 "$NAME" sh -c \
    'dd if=/dev/zero of=/data/fill bs=1M 2>/tmp/fill.err || true; df -h /data; cat /tmp/fill.err' \
    >"$WORK_DIR/fill.txt"

ready_status="$(curl -sS -o "$WORK_DIR/ready.json" -w '%{http_code}' \
    "http://127.0.0.1:$PORT/health/ready")"
health_status="$(curl -sS -o "$WORK_DIR/admin-health.json" -w '%{http_code}' \
    -H "Cookie: $cookies" "http://127.0.0.1:$PORT/api/v1/admin/health")"
write_status="$(curl -sS -o "$WORK_DIR/write.json" -w '%{http_code}' \
    -X POST "http://127.0.0.1:$PORT/api/v1/admin/libraries" \
    -H "Cookie: $cookies" \
    -H "X-CSRF-Token: $csrf_value" \
    -H 'Content-Type: application/json' \
    -d '{"name":"Disk Fault Library","kind":"MOVIE"}')"

[[ "$ready_status" == 503 ]]
[[ "$health_status" == 200 ]]
[[ "$write_status" == 503 ]]
jq -e '.reason == "database_write_unavailable" and .databaseWritable == false' \
    "$WORK_DIR/ready.json" >/dev/null
jq -e '.status == "degraded" and .database.status == "degraded" and .database.writable == false' \
    "$WORK_DIR/admin-health.json" >/dev/null
jq -e '.error.code == "DATABASE_UNAVAILABLE" and (.error.requestId | type == "string")' \
    "$WORK_DIR/write.json" >/dev/null

docker exec --user 0 "$NAME" sh -c 'rm -f /data/fill'
recovery_ready_status="$(curl -sS -o "$WORK_DIR/recovery-ready.json" -w '%{http_code}' \
    "http://127.0.0.1:$PORT/health/ready")"
recovery_health_status="$(curl -sS -o "$WORK_DIR/recovery-health.json" -w '%{http_code}' \
    -H "Cookie: $cookies" "http://127.0.0.1:$PORT/api/v1/admin/health")"
recovery_write_status=503
recovery_library_name=""
for attempt in $(seq 1 10); do
    recovery_library_name="Recovered Library $attempt"
    recovery_write_status="$(curl -sS -o "$WORK_DIR/recovery-write.json" -w '%{http_code}' \
        -X POST "http://127.0.0.1:$PORT/api/v1/admin/libraries" \
        -H "Cookie: $cookies" \
        -H "X-CSRF-Token: $csrf_value" \
        -H 'Content-Type: application/json' \
        -d "{\"name\":\"$recovery_library_name\",\"kind\":\"MOVIE\"}")"
    [[ "$recovery_write_status" == 201 ]] && break
    sleep 0.2
done
[[ "$recovery_ready_status" == 200 ]]
[[ "$recovery_health_status" == 200 ]]
[[ "$recovery_write_status" == 201 ]]
jq -e '.status == "ready" and .databaseWritable == true' \
    "$WORK_DIR/recovery-ready.json" >/dev/null
jq -e '.status == "ok" and .database.status == "ok" and .database.writable == true' \
    "$WORK_DIR/recovery-health.json" >/dev/null
jq -e --arg name "$recovery_library_name" '.library.name == $name' \
    "$WORK_DIR/recovery-write.json" >/dev/null

jq -n \
    --arg architecture "$(uname -m)" \
    --arg image "$IMAGE" \
    --arg readyStatus "$ready_status" \
    --arg healthStatus "$health_status" \
    --arg writeStatus "$write_status" \
    --arg recoveryReadyStatus "$recovery_ready_status" \
    --arg recoveryHealthStatus "$recovery_health_status" \
    --arg recoveryWriteStatus "$recovery_write_status" \
    --argjson ready "$(jq -c . "$WORK_DIR/ready.json")" \
    --argjson adminHealth "$(jq -c . "$WORK_DIR/admin-health.json")" \
    --argjson writeError "$(jq -c . "$WORK_DIR/write.json")" \
    --argjson recoveryReady "$(jq -c . "$WORK_DIR/recovery-ready.json")" \
    --argjson recoveryHealth "$(jq -c . "$WORK_DIR/recovery-health.json")" \
    --argjson recoveryWrite "$(jq -c . "$WORK_DIR/recovery-write.json")" \
    '{architecture: $architecture, image: $image, readyStatus: $readyStatus,
      ready: $ready, adminHealthStatus: $healthStatus, adminHealth: $adminHealth,
      writeStatus: $writeStatus, writeError: $writeError,
      recoveryReadyStatus: $recoveryReadyStatus, recoveryReady: $recoveryReady,
      recoveryHealthStatus: $recoveryHealthStatus, recoveryHealth: $recoveryHealth,
      recoveryWriteStatus: $recoveryWriteStatus, recoveryWrite: $recoveryWrite}'

printf 'diskFill=\n'
cat "$WORK_DIR/fill.txt"
