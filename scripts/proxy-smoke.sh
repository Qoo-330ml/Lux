#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/wait-for-restart.sh
source "$ROOT_DIR/scripts/lib/wait-for-restart.sh"
cd "$ROOT_DIR"

# Static half of the WebSocket check (ADR-054 D12): both nginx samples must carry `Upgrade`/
# `Connection` and send `$http_host`. It needs neither docker nor nginx, so it runs first and fails
# fast on a sample that would make every handshake reach Lux as a plain GET.
"$ROOT_DIR/scripts/check-proxy-config.sh"

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
    -v "$VOLUME:/config" \
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

# A fresh config directory starts in the database-selection mode, so pick SQLite before setup.
lux_select_sqlite_database "https://127.0.0.1:$PORT" -k

setup="$(curl -ksSf -X POST "https://127.0.0.1:$PORT/api/v1/setup/complete" \
    -H 'Content-Type: application/json' \
    -d '{"username":"admin","displayName":"Proxy Smoke Admin","password":"proxy-smoke-test-password","firstLibrary":{"name":"Proxy Smoke Movies","kind":"MOVIE","rootPath":"/media/movies"}}')"
library_id="$(jq -r '.library.id' <<<"$setup")"
if [[ -z "$library_id" || "$library_id" == "null" ]]; then
    echo "setup did not return the first library ID" >&2
    exit 1
fi

# The setup restart closes the upstream port for a moment; nginx answers that window with 502/504,
# which the helper also treats as "down". Wait for the recovery before the login below.
lux_wait_for_restarted_server "https://127.0.0.1:$PORT" -k

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

# --- WebSocket upgrade through nginx (ADR-054 D12, LUX-896) ---------------------------------------
#
# A browser talks to nginx over TLS and nginx forwards a classic HTTP/1.1 upgrade to luxd, so the
# handshake below is exactly what the browser does after `new WebSocket(...)`. This listener uses a
# non-default port, so the `Host: $http_host` sample doubles as a regression test for the port drop
# that `$host` causes: Origin `https://127.0.0.1:$PORT` would stop matching `Host 127.0.0.1`.
ws_origin="https://127.0.0.1:$PORT"
ws_key="$(openssl rand -base64 16)"
ws_accept_expected="$(printf '%s' "${ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11" \
    | openssl dgst -sha1 -binary | openssl base64)"

# `curl` reports the status line of a 101 and then discards the upgraded bytes, so the status and
# handshake headers are read from `-D`. The command always ends with its own timeout because nginx
# keeps the tunnel open; only the captured status matters.
ws_handshake() {
    local name="$1"
    shift
    curl -ksS --http1.1 --max-time 4 \
        -o "$WORK_DIR/ws-$name.body" \
        -D "$WORK_DIR/ws-$name.headers" \
        -w '%{http_code}' \
        "$@" \
        "https://127.0.0.1:$PORT/api/v1/ws" 2>/dev/null || true
}

http_header_value() {
    local file="$1" name="$2"
    grep -i "^${name}:" "$file" | head -n 1 | sed -E 's/^[^:]+:[[:space:]]*//' | tr -d '\r'
}

ws_upgrade_status="$(ws_handshake upgrade \
    -H "Origin: $ws_origin" \
    -H 'Upgrade: websocket' \
    -H 'Connection: Upgrade' \
    -H 'Sec-WebSocket-Version: 13' \
    -H "Sec-WebSocket-Key: $ws_key" \
    -H "Cookie: $cookies")"
if [[ "$ws_upgrade_status" != "101" ]]; then
    echo "WebSocket upgrade through nginx did not return 101: got '$ws_upgrade_status'" >&2
    cat "$WORK_DIR/ws-upgrade.headers" >&2 || true
    exit 1
fi
[[ "$(http_header_value "$WORK_DIR/ws-upgrade.headers" 'upgrade')" == "websocket" ]]
ws_accept_actual="$(http_header_value "$WORK_DIR/ws-upgrade.headers" 'sec-websocket-accept')"
if [[ "$ws_accept_actual" != "$ws_accept_expected" ]]; then
    echo "WebSocket handshake returned an unexpected Sec-WebSocket-Accept: '$ws_accept_actual'" >&2
    exit 1
fi

# The first frame proves the tunnel carries data and not only the status line. `curl` drops
# post-101 bytes, so the frame is read with a short-lived `openssl s_client`; the wait is bounded by
# the polling loop, not by the server's 25 s heartbeat or by the connection staying open.
#
# The frame starts with the binary header (0x81 + length), and an invalid UTF-8 lead byte makes BSD
# grep skip the line even with `-a`, so the payload is matched in the C locale.
ws_frame_contains() {
    local file="$1" pattern="$2"
    LC_ALL=C grep -aq -- "$pattern" "$file"
}
ws_first_frame="$WORK_DIR/ws-first-frame.bin"
(
    printf 'GET /api/v1/ws HTTP/1.1\r\n'
    printf 'Host: 127.0.0.1:%s\r\n' "$PORT"
    printf 'Origin: %s\r\n' "$ws_origin"
    printf 'Upgrade: websocket\r\n'
    printf 'Connection: Upgrade\r\n'
    printf 'Sec-WebSocket-Version: 13\r\n'
    printf 'Sec-WebSocket-Key: %s\r\n' "$ws_key"
    printf 'Cookie: %s\r\n' "$cookies"
    printf '\r\n'
    sleep 2
) | openssl s_client -quiet -connect "127.0.0.1:$PORT" -servername localhost 2>/dev/null \
    >"$ws_first_frame" &
ws_reader_pid=$!
for _ in $(seq 1 20); do
    if ws_frame_contains "$ws_first_frame" '"type":"ready"'; then
        break
    fi
    sleep 0.25
done
kill "$ws_reader_pid" 2>/dev/null || true
wait "$ws_reader_pid" 2>/dev/null || true
if ! ws_frame_contains "$ws_first_frame" '"type":"ready"' \
    || ! ws_frame_contains "$ws_first_frame" '"version":1'; then
    echo "WebSocket upgrade returned 101 but no ready frame" >&2
    exit 1
fi

# Negative cases: a cookie-authenticated handshake without `Origin` must be rejected (ADR-054 D5),
# a mismatched `Origin` must answer 400, and a plain GET — what a proxy that strips `Upgrade` sends
# upstream — must not be upgraded. The plain GET status is recorded instead of asserted: ADR-054's
# endpoint contract says 426 while its reverse-proxy section still mentions the extractor's default
# 400, and the browser turns both into `close 1006`.
ws_missing_origin_status="$(ws_handshake missing-origin \
    -H 'Upgrade: websocket' \
    -H 'Connection: Upgrade' \
    -H 'Sec-WebSocket-Version: 13' \
    -H "Sec-WebSocket-Key: $ws_key" \
    -H "Cookie: $cookies")"
if [[ ! "$ws_missing_origin_status" =~ ^4[0-9][0-9]$ ]]; then
    echo "handshake without Origin was not rejected: got '$ws_missing_origin_status'" >&2
    exit 1
fi
ws_cross_origin_status="$(ws_handshake cross-origin \
    -H 'Origin: https://evil.example' \
    -H 'Upgrade: websocket' \
    -H 'Connection: Upgrade' \
    -H 'Sec-WebSocket-Version: 13' \
    -H "Sec-WebSocket-Key: $ws_key" \
    -H "Cookie: $cookies")"
[[ "$ws_cross_origin_status" == "400" ]]
ws_plain_get_status="$(ws_handshake plain-get \
    -H "Origin: $ws_origin" \
    -H "Cookie: $cookies")"
if [[ ! "$ws_plain_get_status" =~ ^4[0-9][0-9]$ ]]; then
    echo "plain GET on the WebSocket path was not rejected: got '$ws_plain_get_status'" >&2
    exit 1
fi

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
[[ "$remote_login_status" == 200 ]]

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

# The restart canceled the first scan of setup's library and re-queued it; the reconcile below
# would answer `409` (a scan is already running) until that recovered scan finishes.
lux_wait_for_first_scan_settled "https://127.0.0.1:$PORT" "$library_id" -k -H "Cookie: $cookies"

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
    --arg wsUpgradeStatus "$ws_upgrade_status" \
    --arg wsMissingOriginStatus "$ws_missing_origin_status" \
    --arg wsCrossOriginStatus "$ws_cross_origin_status" \
    --arg wsPlainGetStatus "$ws_plain_get_status" \
    '{architecture: $architecture, image: $image, proxyImage: $proxyImage,
      trustedProxySubnet: $subnet, tlsCertificateSubject: $certificateSubject,
      libraryId: $libraryId, jobId: $jobId, itemId: $itemId, sourceId: $sourceId,
      trustedForwardedPublicLoginStatus: $remoteLoginStatus,
      localLoginStatus: $localLoginStatus, rangeStatus: $rangeStatus,
      contentRange: "bytes 2-5/10", responseBody: "2345",
      websocketUpgradeStatus: $wsUpgradeStatus,
      websocketUpgradeFirstFrame: "{\"type\":\"ready\",\"version\":1}",
      websocketMissingOriginStatus: $wsMissingOriginStatus,
      websocketCrossOriginStatus: $wsCrossOriginStatus,
      websocketPlainGetStatus: $wsPlainGetStatus}'
