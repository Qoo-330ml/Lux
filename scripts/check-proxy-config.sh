#!/usr/bin/env bash
# Static check for the reverse-proxy samples that carry Lux's WebSocket upgrade (ADR-054 D12).
#
# `nginx -t` needs nginx and the container smoke needs Docker; neither is available on every
# machine or in every CI job. This check only reads files, so it runs everywhere and guards the
# failure mode ADR-054 calls the most likely one of the migration: a sample that silently loses
# `Upgrade`/`Connection`, or sends `$host` (which drops a non-default port) so the strict
# same-origin check rejects every handshake and browsers only see `close 1006`.
#
# It is intentionally shallow: it cannot prove that nginx forwards the headers. That needs the real
# handshake in `scripts/proxy-smoke.sh` (or a deployment), see docs/COMPATIBILITY.md.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

# `.tmp/` is gitignored and may not exist in a fresh checkout.
mkdir -p "${ROOT_DIR}/.tmp"
WORK_DIR="$(mktemp -d "${ROOT_DIR}/.tmp/proxy-config-check.XXXXXX")"
cleanup() {
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

failures=0

fail() {
    printf 'proxy config check: %s\n' "$*" >&2
    failures=$((failures + 1))
}

require_line() {
    local label="$1" file="$2" pattern="$3" description="$4"
    if ! grep -Eq "$pattern" "$file"; then
        fail "$label: missing ${description}"
    fi
}

forbid_line() {
    local label="$1" file="$2" pattern="$3" description="$4"
    if grep -Eq "$pattern" "$file"; then
        fail "$label: ${description}"
    fi
}

# Both samples must carry the exact directive pair from ADR-054 (`Upgrade` + `Connection`), keep
# HTTP/1.1 to the upstream, disable buffering, and keep `Host` as the browser sent it.
check_nginx_sample() {
    local label="$1" file="$2"
    require_line "$label" "$file" '^[[:space:]]*proxy_http_version 1\.1;' 'proxy_http_version 1.1;'
    require_line "$label" "$file" '^[[:space:]]*proxy_set_header Upgrade \$http_upgrade;' \
        'proxy_set_header Upgrade $http_upgrade;'
    require_line "$label" "$file" '^[[:space:]]*proxy_set_header Connection "upgrade";' \
        'proxy_set_header Connection "upgrade";'
    require_line "$label" "$file" '^[[:space:]]*proxy_set_header Host \$http_host;' \
        'proxy_set_header Host $http_host;'
    require_line "$label" "$file" '^[[:space:]]*proxy_buffering off;' 'proxy_buffering off;'
    require_line "$label" "$file" '^[[:space:]]*proxy_read_timeout ' 'proxy_read_timeout (must exceed the heartbeat interval)'
    forbid_line "$label" "$file" '^[[:space:]]*proxy_set_header Host \$host;' \
        'sends $host instead of $http_host, which drops the non-default port and makes the Origin check reject every handshake'
}

check_nginx_sample 'scripts/proxy-smoke.nginx.conf' "$ROOT_DIR/scripts/proxy-smoke.nginx.conf"

# The deployment sample lives in a fenced code block; extract it so the check stays honest about
# what a reader would copy.
awk '
    /^```nginx[[:space:]]*$/ { inside = 1; next }
    inside && /^```[[:space:]]*$/ { exit }
    inside { print }
' "$ROOT_DIR/docs/DEPLOYMENT.md" >"$WORK_DIR/deployment.nginx.conf"
if [[ ! -s "$WORK_DIR/deployment.nginx.conf" ]]; then
    fail 'docs/DEPLOYMENT.md: no ```nginx sample block found'
else
    check_nginx_sample 'docs/DEPLOYMENT.md nginx 样例' "$WORK_DIR/deployment.nginx.conf"
fi

if [[ "$failures" -ne 0 ]]; then
    printf 'proxy config check: %d problem(s)\n' "$failures" >&2
    exit 1
fi

printf 'proxy config check: nginx samples carry Upgrade/Connection, Host $http_host, HTTP/1.1 and no buffering\n'
