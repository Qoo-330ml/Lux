#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

IMAGE="${LUX_IMAGE:-lux:arm64-local}"
PORT="${LUX_MOUNT_LOSS_PORT:-18609}"
NAME="${LUX_MOUNT_LOSS_CONTAINER:-lux-mount-loss}"
VOLUME="${LUX_MOUNT_LOSS_VOLUME:-lux-mount-loss-data}"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lux-mount-loss.XXXXXX")"
MEDIA_DIR="$WORK_DIR/media"
MEDIA_ROOT="$MEDIA_DIR/movies"
COOKIE_HEADERS="$WORK_DIR/login.headers"

cleanup() {
    chmod 755 "$MEDIA_ROOT" >/dev/null 2>&1 || true
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    docker volume rm "$VOLUME" >/dev/null 2>&1 || true
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

for command in curl docker jq; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "required command not found: $command" >&2
        exit 1
    fi
done

mkdir -p "$MEDIA_ROOT"
chmod 755 "$MEDIA_ROOT"
printf 'mount-loss-fixture' >"$MEDIA_ROOT/Recovery.Movie.2024.mkv"

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker volume rm "$VOLUME" >/dev/null 2>&1 || true
docker volume create "$VOLUME" >/dev/null
docker run -d --rm \
    --name "$NAME" \
    -p "$PORT:8097" \
    -v "$VOLUME:/data" \
    -v "$MEDIA_DIR:/media:rw" \
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

setup="$(curl -fsS -X POST "http://127.0.0.1:$PORT/api/v1/setup/complete" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","displayName":"Mount Fault Admin","password":"mount-loss-test-password","firstLibrary":{"name":"Mount Fault Movies","kind":"MOVIE","rootPath":"/media/movies"}}')"
library_id="$(jq -r '.library.id' <<<"$setup")"
if [[ -z "$library_id" || "$library_id" == "null" ]]; then
    echo "setup did not return the first library ID" >&2
    exit 1
fi

curl -sS -D "$COOKIE_HEADERS" -o "$WORK_DIR/login.json" \
    -X POST "http://127.0.0.1:$PORT/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"mount-loss-test-password"}'
session_cookie="$(grep -i '^set-cookie: lux_session=' "$COOKIE_HEADERS" \
    | sed -E 's/^[^:]+: (lux_session=[^;]+).*/\1/I' | tail -n 1)"
csrf_cookie="$(grep -i '^set-cookie: lux_csrf=' "$COOKIE_HEADERS" \
    | sed -E 's/^[^:]+: (lux_csrf=[^;]+).*/\1/I' | tail -n 1)"
if [[ -z "$session_cookie" || -z "$csrf_cookie" ]]; then
    echo "login did not return both session cookies" >&2
    exit 1
fi
csrf_value="${csrf_cookie#lux_csrf=}"
cookies="$session_cookie; $csrf_cookie"

start_reconcile() {
    curl -fsS -X POST "http://127.0.0.1:$PORT/api/v1/admin/libraries/$library_id/reconcile" \
        -H "Cookie: $cookies" \
        -H "X-CSRF-Token: $csrf_value"
}

wait_for_job() {
    local job_id="$1"
    local body=""
    local status=""
    for _ in $(seq 1 100); do
        body="$(curl -fsS -H "Cookie: $cookies" \
            "http://127.0.0.1:$PORT/api/v1/admin/jobs/$job_id")"
        status="$(jq -r '.job.status' <<<"$body")"
        if [[ "$status" == "COMPLETED" ]]; then
            return 0
        fi
        if [[ "$status" == "FAILED" || "$status" == "CANCELLED" ]]; then
            echo "reconcile ended unexpectedly: $body" >&2
            return 1
        fi
        sleep 0.1
    done
    echo "reconcile did not complete: $body" >&2
    return 1
}

first_job="$(start_reconcile)"
first_job_id="$(jq -r '.job.id' <<<"$first_job")"
wait_for_job "$first_job_id"

initial_library="$(curl -fsS -H "Cookie: $cookies" \
    "http://127.0.0.1:$PORT/api/v1/admin/libraries")"
jq -e --arg id "$library_id" \
    '.libraries[] | select(.id == $id) | .roots[0].isAvailable == true' \
    <<<"$initial_library" >/dev/null
initial_items="$(curl -fsS -H "Cookie: $cookies" \
    "http://127.0.0.1:$PORT/api/v1/libraries/$library_id/items?page=1&pageSize=50")"
jq -e '(.items | length) >= 1' <<<"$initial_items" >/dev/null

chmod 000 "$MEDIA_ROOT"
unavailable_job="$(start_reconcile)"
unavailable_job_id="$(jq -r '.job.id' <<<"$unavailable_job")"
wait_for_job "$unavailable_job_id"
unavailable_library="$(curl -fsS -H "Cookie: $cookies" \
    "http://127.0.0.1:$PORT/api/v1/admin/libraries")"
unavailable_items="$(curl -fsS -H "Cookie: $cookies" \
    "http://127.0.0.1:$PORT/api/v1/libraries/$library_id/items?page=1&pageSize=50")"
jq -e --arg id "$library_id" \
    '.libraries[] | select(.id == $id) | .roots[0].isAvailable == false' \
    <<<"$unavailable_library" >/dev/null
jq -e '(.items | length) >= 1' <<<"$unavailable_items" >/dev/null

chmod 755 "$MEDIA_ROOT"
recovered_job="$(start_reconcile)"
recovered_job_id="$(jq -r '.job.id' <<<"$recovered_job")"
wait_for_job "$recovered_job_id"
recovered_library="$(curl -fsS -H "Cookie: $cookies" \
    "http://127.0.0.1:$PORT/api/v1/admin/libraries")"
recovered_items="$(curl -fsS -H "Cookie: $cookies" \
    "http://127.0.0.1:$PORT/api/v1/libraries/$library_id/items?page=1&pageSize=50")"
jq -e --arg id "$library_id" \
    '.libraries[] | select(.id == $id) | .roots[0].isAvailable == true' \
    <<<"$recovered_library" >/dev/null
jq -e '(.items | length) >= 1' <<<"$recovered_items" >/dev/null

jq -n \
    --arg architecture "$(uname -m)" \
    --arg image "$IMAGE" \
    --arg libraryId "$library_id" \
    --arg initialJobId "$first_job_id" \
    --arg unavailableJobId "$unavailable_job_id" \
    --arg recoveredJobId "$recovered_job_id" \
    '{architecture: $architecture, image: $image, libraryId: $libraryId,
      initialJobId: $initialJobId, initialRootAvailable: true,
      unavailableJobId: $unavailableJobId, unavailableRootAvailable: false,
      itemsPreservedWhileUnavailable: true,
      recoveredJobId: $recoveredJobId, recoveredRootAvailable: true,
      itemsPresentAfterRecovery: true}'
