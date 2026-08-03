#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

IMAGE="${LUX_IMAGE:-lux:arm64-local}"
PROXY_IMAGE="${LUX_PROXY_IMAGE:-nginx:1.27-alpine}"
PORT="${LUX_PROXY_SMOKE_PORT:-18611}"
NETWORK="${LUX_PROXY_SMOKE_NETWORK:-lux-proxy-smoke-net}"
VOLUME="${LUX_PROXY_SMOKE_VOLUME:-lux-proxy-smoke-data}"
UPSTREAM_NAME="${LUX_PROXY_SMOKE_UPSTREAM:-lux-proxy-upstream}"
PROXY_NAME="${LUX_PROXY_SMOKE_CONTAINER:-lux-proxy-smoke}"
SUBNET="${LUX_PROXY_SMOKE_SUBNET:-172.30.77.0/24}"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lux-proxy-smoke.XXXXXX")"
MEDIA_DIR="$WORK_DIR/media"
MEDIA_ROOT="$MEDIA_DIR/movies"
COOKIE_HEADERS="$WORK_DIR/login.headers"

cleanup() {
    docker rm -f "$PROXY_NAME" >/dev/null 2>&1 || true
    docker rm -f "$UPSTREAM_NAME" >/dev/null 2>&1 || true
    docker network rm "$NETWORK" >/dev/null 2>&1 || true
    docker volume rm "$VOLUME" >/dev/null 2>&1 || true
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

for command in curl docker jq openssl; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "required command not found: $command" >&2
        exit 1
    fi
done

mkdir -p "$MEDIA_ROOT"
chmod 755 "$MEDIA_DIR" "$MEDIA_ROOT"
printf '0123456789' >"$MEDIA_ROOT/Proxy.Range.2024.mkv"

openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$WORK_DIR/key.pem" \
    -out "$WORK_DIR/cert.pem" \
    -days 1 \
    -subj '/CN=localhost' \
    >/dev/null 2>&1

docker rm -f "$PROXY_NAME" "$UPSTREAM_NAME" >/dev/null 2>&1 || true
docker network rm "$NETWORK" >/dev/null 2>&1 || true
docker volume rm "$VOLUME" >/dev/null 2>&1 || true
docker network create --driver bridge --subnet "$SUBNET" "$NETWORK" >/dev/null
docker volume create "$VOLUME" >/dev/null

docker run -d --rm \
    --name "$UPSTREAM_NAME" \
    --network "$NETWORK" \
    --network-alias lux-proxy-upstream \
    -e "LUX_TRUSTED_PROXY_CIDRS=$SUBNET" \
    -v "$VOLUME:/data" \
    -v "$MEDIA_DIR:/media:rw" \
    "$IMAGE" >"$WORK_DIR/upstream.id"

docker run -d --rm --pull=missing \
    --name "$PROXY_NAME" \
    --network "$NETWORK" \
    -p "$PORT:443" \
    -v "$ROOT_DIR/scripts/proxy-smoke.nginx.conf:/etc/nginx/conf.d/default.conf:ro" \
    -v "$WORK_DIR/cert.pem:/etc/nginx/certs/cert.pem:ro" \
    -v "$WORK_DIR/key.pem:/etc/nginx/certs/key.pem:ro" \
    "$PROXY_IMAGE" >"$WORK_DIR/proxy.id"

started=0
for _ in $(seq 1 45); do
    if curl -ksSf "https://127.0.0.1:$PORT/health/live" >/dev/null 2>&1; then
        started=1
        break
    fi
    sleep 1
done
if [[ "$started" != 1 ]]; then
    docker logs "$PROXY_NAME" >&2 || true
    docker logs "$UPSTREAM_NAME" >&2 || true
    exit 1
fi

served_subject="$(printf '' | openssl s_client -connect "127.0.0.1:$PORT" -servername localhost 2>/dev/null | openssl x509 -noout -subject)"
grep -q 'localhost' <<<"$served_subject"

setup="$(curl -ksSf -X POST "https://127.0.0.1:$PORT/api/v1/setup/complete" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","displayName":"Proxy Smoke Admin","password":"proxy-smoke-test-password","firstLibrary":{"name":"Proxy Smoke Movies","kind":"MOVIE","rootPath":"/media/movies"}}')"
library_id="$(jq -r '.library.id' <<<"$setup")"
if [[ -z "$library_id" || "$library_id" == "null" ]]; then
    echo "setup did not return the first library ID" >&2
    exit 1
fi

curl -ksS -D "$COOKIE_HEADERS" -o "$WORK_DIR/login.json" \
    -X POST "https://127.0.0.1:$PORT/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","password":"proxy-smoke-test-password"}'
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

create_viewer_status="$(curl -ksS -o "$WORK_DIR/viewer.json" -w '%{http_code}' \
    -X POST "https://127.0.0.1:$PORT/api/v1/admin/users" \
    -H "Cookie: $cookies" \
    -H "X-CSRF-Token: $csrf_value" \
    -H 'Content-Type: application/json' \
    -d '{"username":"proxy-viewer","displayName":"Proxy Viewer","password":"proxy-viewer-password"}')"
[[ "$create_viewer_status" == 201 ]]

remote_login_status="$(curl -ksS -o "$WORK_DIR/remote-login.json" -w '%{http_code}' \
    -X POST "https://127.0.0.1:$PORT/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -H 'X-Forwarded-For: 8.8.8.8' \
    -d '{"username":"proxy-viewer","password":"proxy-viewer-password"}')"
[[ "$remote_login_status" == 403 ]]

local_login_status="$(curl -ksS -o "$WORK_DIR/local-login.json" -w '%{http_code}' \
    -X POST "https://127.0.0.1:$PORT/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d '{"username":"proxy-viewer","password":"proxy-viewer-password"}')"
[[ "$local_login_status" == 200 ]]

start_reconcile() {
    curl -ksSf -X POST "https://127.0.0.1:$PORT/api/v1/admin/libraries/$library_id/reconcile" \
        -H "Cookie: $cookies" \
        -H "X-CSRF-Token: $csrf_value"
}

wait_for_job() {
    local job_id="$1"
    local body=""
    local status=""
    for _ in $(seq 1 100); do
        body="$(curl -ksSf -H "Cookie: $cookies" \
            "https://127.0.0.1:$PORT/api/v1/admin/jobs/$job_id")"
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

reconcile="$(start_reconcile)"
job_id="$(jq -r '.job.id' <<<"$reconcile")"
wait_for_job "$job_id"

items="$(curl -ksSf -H "Cookie: $cookies" \
    "https://127.0.0.1:$PORT/api/v1/libraries/$library_id/items?page=1&pageSize=50")"
item_id="$(jq -r '.items[0].id' <<<"$items")"
source_id="$(jq -r '.items[0].mediaSources[0].id' <<<"$items")"
if [[ -z "$item_id" || "$item_id" == "null" || -z "$source_id" || "$source_id" == "null" ]]; then
    echo "reconcile did not expose a playable source: $items" >&2
    exit 1
fi

range_headers="$WORK_DIR/range.headers"
range_body="$WORK_DIR/range.body"
range_status="$(curl -ksS -D "$range_headers" -o "$range_body" -w '%{http_code}' \
    -H "Cookie: $cookies" \
    -H 'Range: bytes=2-5' \
    "https://127.0.0.1:$PORT/api/v1/items/$item_id/stream?sourceId=$source_id")"
[[ "$range_status" == 206 ]]
grep -Eiq '^content-range:[[:space:]]*bytes 2-5/10' "$range_headers"
grep -Eiq '^content-length:[[:space:]]*4' "$range_headers"
grep -Eiq '^accept-ranges:[[:space:]]*bytes' "$range_headers"
grep -Eiq '^etag:' "$range_headers"
[[ "$(<"$range_body")" == "2345" ]]

jq -n \
    --arg architecture "$(uname -m)" \
    --arg image "$IMAGE" \
    --arg proxyImage "$PROXY_IMAGE" \
    --arg subnet "$SUBNET" \
    --arg certificateSubject "$served_subject" \
    --arg libraryId "$library_id" \
    --arg jobId "$job_id" \
    --arg itemId "$item_id" \
    --arg sourceId "$source_id" \
    --arg remoteLoginStatus "$remote_login_status" \
    --arg localLoginStatus "$local_login_status" \
    --arg rangeStatus "$range_status" \
    '{architecture: $architecture, image: $image, proxyImage: $proxyImage,
      trustedProxySubnet: $subnet, tlsCertificateSubject: $certificateSubject,
      libraryId: $libraryId, jobId: $jobId, itemId: $itemId, sourceId: $sourceId,
      trustedForwardedPublicLoginStatus: $remoteLoginStatus,
      localLoginStatus: $localLoginStatus, rangeStatus: $rangeStatus,
      contentRange: "bytes 2-5/10", responseBody: "2345"}'

