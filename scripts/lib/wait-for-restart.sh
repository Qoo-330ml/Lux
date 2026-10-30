#!/usr/bin/env bash
#
# Shared wait helpers for the smoke scripts that initialize an instance through
# `POST /api/v1/setup/complete`.
#
# That request now asks the process to restart itself a moment after the `201` (design:
# `docs/SETUP-RESTART-DESIGN.md`), so every script that keeps talking to the same instance has to
# wait for the restart window instead of racing it. Do not paper over this with `sleep`: the wait
# has to observe the restart and the recovery, and it has to fail loudly when the instance never
# comes back.
#
# The restart also cancels the first scan that setup started and queues it again after the restart,
# which is why scripts that immediately start their own scan/reconcile must wait for that scan to
# settle first (the scan endpoints answer `409` while a scan is active).
#
# Usage (after `set -euo pipefail`):
#
#   source "$ROOT_DIR/scripts/lib/wait-for-restart.sh"
#   lux_wait_for_restarted_server "http://127.0.0.1:$PORT"
#   lux_wait_for_restarted_server "https://127.0.0.1:$PORT" -k
#   lux_wait_for_first_scan_settled "http://127.0.0.1:$PORT" "$library_id" -H "Cookie: $cookies"
#   lux_wait_for_first_scan_settled "http://127.0.0.1:$PORT" "$library_id" -b "$COOKIE_JAR"
#
# Tunables (seconds):
#   LUX_RESTART_DOWN_TIMEOUT  how long to look for the restart window (default 15)
#   LUX_RESTART_UP_TIMEOUT    how long to wait for `/health/ready` afterwards (default 120)
#   LUX_SETUP_FIRST_SCAN_APPEAR_TIMEOUT  how long to look for the re-queued first scan (default 30)
#   LUX_SETUP_FIRST_SCAN_TIMEOUT         how long that scan may run (default 600)
#
# Every helper returns non-zero with a message on stderr when the deadline passes.

# Prints the HTTP status of a URL, or `000` when the request could not be made.
#
# Extra arguments are passed to `curl` (the proxy smoke script needs `-k`).
lux_restart_http_status() {
    local url="$1"
    shift
    local code
    code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$@" "$url" || true)"
    if [[ -z "$code" ]]; then
        code="000"
    fi
    printf '%s' "$code"
}

# Selects SQLite for a fresh instance.
#
# A fresh config directory starts in the database-selection mode (`setup/complete` answers
# `409 DATABASE_CONFIGURATION_REQUIRED`), so every smoke script has to pick a backend first. SQLite
# needs no extra configuration and keeps the scripts self-contained.
lux_select_sqlite_database() {
    local base_url="${1:?usage: lux_select_sqlite_database BASE_URL [curl flags...]}"
    shift
    local body
    body="$(curl -s -f -X POST "$@" -H 'Content-Type: application/json' \
        -d '{"backend":"SQLITE"}' "$base_url/api/v1/setup/database/select" || true)"
    if [[ -z "$body" ]]; then
        echo "error: $base_url/api/v1/setup/database/select did not select SQLite" >&2
        return 1
    fi
    local restart_required
    restart_required="$(jq -r '.restartRequired // false' <<<"$body" 2>/dev/null || true)"
    if [[ "$restart_required" != "false" ]]; then
        echo "error: selecting SQLite unexpectedly requires a restart: $body" >&2
        return 1
    fi
}

# Waits until the instance finished the restart that `setup/complete` requested.
#
# Phase 1 watches `/health/live` until a request fails (the port is closed while the process
# replaces itself). A full timeout without any failure means the restart already happened before we
# looked, or the instance runs without a restart handle; the helper warns and continues, and phase 2
# still proves the server answers.
lux_wait_for_restarted_server() {
    local base_url="${1:?usage: lux_wait_for_restarted_server BASE_URL [curl flags...]}"
    shift
    local down_timeout="${LUX_RESTART_DOWN_TIMEOUT:-15}"
    local up_timeout="${LUX_RESTART_UP_TIMEOUT:-120}"
    local deadline status
    local saw_down=0

    deadline=$((SECONDS + down_timeout))
    while ((SECONDS < deadline)); do
        status="$(lux_restart_http_status "$base_url/health/live" "$@")"
        if [[ "$status" != "200" ]]; then
            saw_down=1
            break
        fi
        sleep 0.05
    done
    if ((saw_down != 1)); then
        echo "warning: $base_url never stopped answering within ${down_timeout}s; assuming the restart already happened" >&2
    fi

    deadline=$((SECONDS + up_timeout))
    while ((SECONDS < deadline)); do
        status="$(lux_restart_http_status "$base_url/health/ready" "$@")"
        if [[ "$status" == "200" ]]; then
            return 0
        fi
        sleep 0.1
    done
    echo "error: $base_url/health/ready did not become ready within ${up_timeout}s after the setup restart (last status: ${status:-none}); check the instance logs" >&2
    return 1
}

# Prints `active` while the library has a queued or running scan, `done` when it was scanned, and
# `unknown` when neither is observable yet (or a request failed).
lux_restart_library_scan_state() {
    local base_url="$1"
    local library_id="$2"
    shift 2
    local jobs active last_scan libraries

    jobs="$(curl -s --max-time 10 "$@" \
        "$base_url/api/v1/admin/jobs?page=1&pageSize=100" || true)"
    if [[ -z "$jobs" ]]; then
        printf 'unknown'
        return 0
    fi
    active="$(jq -r --arg id "$library_id" \
        '[.jobs[]? | select(.libraryId == $id and (.status == "PENDING" or .status == "RUNNING"))] | length' \
        <<<"$jobs" 2>/dev/null || true)"
    if [[ "$active" != "0" && "$active" != "" ]]; then
        printf 'active'
        return 0
    fi
    if [[ "$active" != "0" ]]; then
        printf 'unknown'
        return 0
    fi

    libraries="$(curl -s --max-time 10 "$@" \
        "$base_url/api/v1/admin/libraries" || true)"
    if [[ -z "$libraries" ]]; then
        printf 'unknown'
        return 0
    fi
    last_scan="$(jq -r --arg id "$library_id" \
        '.libraries[]? | select(.id == $id) | .lastScanAt' <<<"$libraries" 2>/dev/null || true)"
    if [[ -n "$last_scan" && "$last_scan" != "null" ]]; then
        printf 'done'
    else
        printf 'unknown'
    fi
}

# Prints the id of the scan job that is currently queued or running for a library.
#
# The restart re-queues the first scan setup started, so a drill that wants to interrupt a real scan
# asks for the job that exists now instead of the `scanJob` of the `201` (which the restart already
# cancelled).
lux_wait_for_active_scan_job() {
    local base_url="${1:?usage: lux_wait_for_active_scan_job BASE_URL LIBRARY_ID [auth curl flags...]}"
    local library_id="${2:?library id required}"
    shift 2
    local timeout="${LUX_SETUP_FIRST_SCAN_APPEAR_TIMEOUT:-30}"
    local deadline jobs job_id

    deadline=$((SECONDS + timeout))
    while ((SECONDS < deadline)); do
        jobs="$(curl -s --max-time 10 "$@" \
            "$base_url/api/v1/admin/jobs?page=1&pageSize=100" || true)"
        if [[ -n "$jobs" ]]; then
            job_id="$(jq -r --arg id "$library_id" \
                '[.jobs[]? | select(.libraryId == $id and (.status == "PENDING" or .status == "RUNNING"))]
                 | sort_by([.createdAt, .id]) | last | .id // ""' \
                <<<"$jobs" 2>/dev/null || true)"
            if [[ -n "$job_id" && "$job_id" != "null" ]]; then
                printf '%s' "$job_id"
                return 0
            fi
        fi
        sleep 0.1
    done
    echo "error: no queued or running scan appeared for library $library_id within ${timeout}s" >&2
    return 1
}

# Waits until the first scan that the restart re-queued has finished.
#
# The cancellation and the re-queue are the documented behaviour (`first scan` recovery), so a
# script that wants to drive a scan of its own has to wait for that recovery scan; otherwise the
# scan endpoints answer `409` and the `201`/`scanJob` of setup point at a job that no longer exists.
lux_wait_for_first_scan_settled() {
    local base_url="${1:?usage: lux_wait_for_first_scan_settled BASE_URL LIBRARY_ID [auth curl flags...]}"
    local library_id="${2:?library id required}"
    shift 2
    local appear_timeout="${LUX_SETUP_FIRST_SCAN_APPEAR_TIMEOUT:-30}"
    local settle_timeout="${LUX_SETUP_FIRST_SCAN_TIMEOUT:-600}"
    local deadline state

    deadline=$((SECONDS + appear_timeout))
    state=""
    while ((SECONDS < deadline)); do
        state="$(lux_restart_library_scan_state "$base_url" "$library_id" "$@")"
        case "$state" in
            active) break ;;
            done) return 0 ;;
        esac
        sleep 0.1
    done
    if [[ "$state" != "active" ]]; then
        echo "warning: no re-queued first scan appeared for library $library_id within ${appear_timeout}s; continuing" >&2
        return 0
    fi

    deadline=$((SECONDS + settle_timeout))
    while ((SECONDS < deadline)); do
        state="$(lux_restart_library_scan_state "$base_url" "$library_id" "$@")"
        if [[ "$state" != "active" ]]; then
            return 0
        fi
        sleep 0.2
    done
    echo "error: the first scan of library $library_id is still running after ${settle_timeout}s; the restart recovery did not settle" >&2
    return 1
}
