#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LUX_HTTP_ADDR="${LUX_HTTP_ADDR:-0.0.0.0:8097}"
LUX_CONFIG_DIR="${LUX_CONFIG_DIR:-./config}"
RUST_LOG="${RUST_LOG:-luxd=info,tower_http=info}"
PORT="${LUX_HTTP_ADDR##*:}"

cd "$ROOT_DIR"

log() {
    printf '[lux] %s\n' "$*"
}

if ! command -v lsof >/dev/null 2>&1; then
    printf '[lux] lsof is required to find the process listening on %s\n' "$PORT" >&2
    exit 1
fi

listener_pids() {
    lsof -nP -t -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true
}

process_cwd() {
    lsof -a -p "$1" -d cwd -Fn 2>/dev/null \
        | sed -n 's/^n//p' \
        | sed -n '1p'
}

is_local_lux() {
    local pid="$1"
    local command cwd

    command="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    cwd="$(process_cwd "$pid")"
    [[ "$command" == *luxd* && "$cwd" == "$ROOT_DIR" ]]
}

lux_pids=()
while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    if ! is_local_lux "$pid"; then
        command="$(ps -p "$pid" -o command= 2>/dev/null || printf 'unknown')"
        printf '[lux] refusing to stop another process on port %s: %s\n' "$PORT" "$command" >&2
        exit 1
    fi
    lux_pids+=("$pid")
done < <(listener_pids)

if ((${#lux_pids[@]} > 0)); then
    log "stopping existing Lux process: ${lux_pids[*]}"
    kill "${lux_pids[@]}"

    deadline=$((SECONDS + 10))
    while :; do
        still_running=()
        for pid in "${lux_pids[@]}"; do
            if kill -0 "$pid" 2>/dev/null; then
                still_running+=("$pid")
            fi
        done
        ((${#still_running[@]} == 0)) && break
        if ((SECONDS >= deadline)); then
            log "Lux did not stop gracefully; forcing exit: ${still_running[*]}"
            kill -KILL "${still_running[@]}"
            break
        fi
        sleep 0.25
    done
fi

log "building Web frontend"
pnpm --dir web install --frozen-lockfile
pnpm --dir web build

log "compiling and starting Lux on ${LUX_HTTP_ADDR}"
exec env \
    RUST_LOG="$RUST_LOG" \
    LUX_HTTP_ADDR="$LUX_HTTP_ADDR" \
    LUX_CONFIG_DIR="$LUX_CONFIG_DIR" \
    cargo run --locked --bin luxd
