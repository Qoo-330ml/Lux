#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

IMAGE="${LUX_IMAGE:-lux:arm64-local}"
PORT="${LUX_PROBE_SMOKE_PORT:-18613}"
NAME="${LUX_PROBE_SMOKE_CONTAINER:-lux-probe-smoke}"
VOLUME="${LUX_PROBE_SMOKE_VOLUME:-lux-probe-smoke-data}"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/lux-probe-smoke.XXXXXX")"
MEDIA_DIR="$WORK_DIR/media"
MEDIA_ROOT="$MEDIA_DIR/movies"
COOKIE_HEADERS="$WORK_DIR/login.headers"
PASSWORD="probe-smoke-test-password"

cleanup() {
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
chmod 755 "$MEDIA_DIR" "$MEDIA_ROOT"

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker volume rm "$VOLUME" >/dev/null 2>&1 || true
docker volume create "$VOLUME" >/dev/null

docker run --rm --platform linux/arm64 --user 0 --entrypoint ffmpeg \
    -v "$MEDIA_DIR:/media:rw" "$IMAGE" \
    -hide_banner -loglevel error \
    -f lavfi -i color=c=black:s=320x180:d=1 \
    -f lavfi -i anullsrc=r=48000:cl=mono -t 1 \
    -c:v libx264 -pix_fmt yuv420p -c:a aac -shortest \
    "/media/movies/Probe.Movie.2024.mp4" -y

docker run -d --rm --platform linux/arm64 \
    --name "$NAME" \
    -p "$PORT:8097" \
    -v "$VOLUME:/config" \
    -v "$MEDIA_DIR:/media:rw" \
    "$IMAGE" >"$WORK_DIR/container.id"

started=0
for _ in $(seq 1 45); do
    if curl -fsS "http://127.0.0.1:$PORT/health/live" >/dev/null 2>&1; then
        started=1
        break
    fi
    sleep 1
done
if [[ "$started" != 1 ]]; then
    docker logs "$NAME" >&2 || true
    exit 1
fi

setup="$(curl -fsS -X POST "http://127.0.0.1:$PORT/api/v1/setup/complete" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"displayName\":\"Probe Smoke Admin\",\"password\":\"$PASSWORD\",\"firstLibrary\":{\"name\":\"Probe Smoke Movies\",\"kind\":\"MOVIE\",\"rootPath\":\"/media/movies\"}}")"
library_id="$(jq -r '.library.id' <<<"$setup")"
if [[ -z "$library_id" || "$library_id" == "null" ]]; then
    echo "setup did not return the first library ID" >&2
    exit 1
fi

curl -sS -D "$COOKIE_HEADERS" -o "$WORK_DIR/login.json" \
    -X POST "http://127.0.0.1:$PORT/api/v1/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$PASSWORD\"}"
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

reconcile="$(curl -fsS -X POST \
    "http://127.0.0.1:$PORT/api/v1/admin/libraries/$library_id/reconcile" \
    -H "Cookie: $cookies" \
    -H "X-CSRF-Token: $csrf_value")"
job_id="$(jq -r '.job.id' <<<"$reconcile")"

job_body=""
job_status=""
for _ in $(seq 1 100); do
    job_body="$(curl -fsS -H "Cookie: $cookies" \
        "http://127.0.0.1:$PORT/api/v1/admin/jobs/$job_id")"
    job_status="$(jq -r '.job.status' <<<"$job_body")"
    if [[ "$job_status" == "COMPLETED" ]]; then
        break
    fi
    if [[ "$job_status" == "FAILED" || "$job_status" == "CANCELLED" ]]; then
        echo "reconcile ended unexpectedly: $job_body" >&2
        exit 1
    fi
    sleep 0.1
done
if [[ "$job_status" != "COMPLETED" ]]; then
    echo "reconcile did not complete: $job_body" >&2
    exit 1
fi

items=""
probe_status=""
for _ in $(seq 1 100); do
    items="$(curl -fsS -H "Cookie: $cookies" \
        "http://127.0.0.1:$PORT/api/v1/libraries/$library_id/items?page=1&pageSize=50")"
    probe_status="$(jq -r '.items[0].mediaSources[0].probeStatus // ""' <<<"$items")"
    if [[ "$probe_status" == "READY" ]]; then
        break
    fi
    if [[ "$probe_status" == "FAILED" ]]; then
        echo "ffprobe failed: $items" >&2
        docker logs "$NAME" >&2 || true
        exit 1
    fi
    sleep 0.1
done
if [[ "$probe_status" != "READY" ]]; then
    echo "scan completed without a ready probe: $items" >&2
    docker logs "$NAME" >&2 || true
    exit 1
fi

item_id="$(jq -r '.items[0].id' <<<"$items")"
source_id="$(jq -r '.items[0].mediaSources[0].id' <<<"$items")"
emby_token="$(curl -fsS \
    -X POST "http://127.0.0.1:$PORT/Users/AuthenticateByName" \
    -H 'Authorization: Emby Client="Probe Smoke", Device="ARM64", DeviceId="probe-smoke", Version="1"' \
    -H 'Content-Type: application/json' \
    -d "{\"Username\":\"admin\",\"Pw\":\"$PASSWORD\"}" \
    | jq -r '.AccessToken')"
if [[ -z "$emby_token" || "$emby_token" == "null" ]]; then
    echo "Emby authentication did not return an access token" >&2
    exit 1
fi
playback="$(curl -fsS \
    "http://127.0.0.1:$PORT/Items/$item_id/PlaybackInfo?api_key=$emby_token")"

jq -e --arg source_id "$source_id" \
    '(.MediaSources | length) >= 1 and
     .MediaSources[0].Id == $source_id and
     (.MediaSources[0].RunTimeTicks // 0) > 0 and
     (.MediaSources[0].MediaStreams | length) > 0 and
     (.MediaSources[0].Container | contains("mp4"))' <<<"$playback" >/dev/null

events="$(curl -fsS -H "Cookie: $cookies" \
    "http://127.0.0.1:$PORT/api/v1/admin/jobs/$job_id/events?page=1&pageSize=100")"
jq -e 'any(.events[]?; .eventCode == "PROBE_COMPLETED")' <<<"$events" >/dev/null

jq -n \
    --arg architecture "$(uname -m)" \
    --arg image "$IMAGE" \
    --arg libraryId "$library_id" \
    --arg jobId "$job_id" \
    --arg itemId "$item_id" \
    --arg sourceId "$source_id" \
    --arg probeStatus "$probe_status" \
    --argjson items "$items" \
    --argjson playback "$playback" \
    '{architecture: $architecture, image: $image, libraryId: $libraryId,
      jobId: $jobId, itemId: $itemId, sourceId: $sourceId,
      probeStatus: $probeStatus,
      durationTicks: $items.items[0].mediaSources[0].durationTicks,
      playbackRunTimeTicks: $playback.MediaSources[0].RunTimeTicks,
      playbackStreamCount: ($playback.MediaSources[0].MediaStreams | length),
      playbackContainer: $playback.MediaSources[0].Container,
      probeEventRecorded: true}'
