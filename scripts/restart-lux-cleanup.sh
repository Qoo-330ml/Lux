#!/usr/bin/env bash

cleanup_old_target_if_stale() {
    local target_dir="${1:-}"
    local min_age_minutes="${2:-1440}"
    local cargo_command
    local recent_file

    shift 2 || true
    cargo_command=("$@")

    if [[ -z "$target_dir" || "$target_dir" == "/" || "$target_dir" == "." ]]; then
        printf '[lux] refusing to clean an unsafe target directory: %s\n' "$target_dir" >&2
        return 2
    fi

    if [[ ! "$min_age_minutes" =~ ^[1-9][0-9]*$ ]]; then
        printf '[lux] invalid cleanup age in minutes: %s\n' "$min_age_minutes" >&2
        return 2
    fi

    if ((${#cargo_command[@]} == 0)); then
        printf '%s\n' '[lux] no Cargo command was provided for target cleanup' >&2
        return 2
    fi

    [[ -d "$target_dir" ]] || return 0

    recent_file="$(find "$target_dir" -type f -mmin "-$min_age_minutes" -print -quit)"
    [[ -z "$recent_file" ]] || return 0

    "${cargo_command[@]}" clean --locked --target-dir "$target_dir"
}

cleanup_stale_git_temp_packs() {
    local temp_root="${1:-${TMPDIR:-/tmp}}"
    local min_age_minutes="${2:-60}"
    local temp_root_normalized
    local pack
    local stale_pack
    local open_pids
    local removed_count=0

    if [[ ! "$min_age_minutes" =~ ^[1-9][0-9]*$ ]]; then
        printf '[lux] invalid Git temporary-pack age in minutes: %s\n' "$min_age_minutes" >&2
        return 2
    fi

    temp_root_normalized="${temp_root%/}"
    if [[ -z "$temp_root_normalized" || "$temp_root_normalized" == "/" ]]; then
        printf '[lux] refusing to clean an unsafe temporary directory: %s\n' "$temp_root" >&2
        return 2
    fi

    if ! command -v lsof >/dev/null 2>&1; then
        printf '%s\n' '[lux] lsof is required to protect active Git temporary packs' >&2
        return 2
    fi

    [[ -d "$temp_root_normalized" ]] || return 0

    for pack in "$temp_root_normalized"/tmp.*/objects/pack/tmp_pack_*; do
        [[ -f "$pack" ]] || continue
        stale_pack="$(find "$pack" -type f -mmin "+$min_age_minutes" -print -quit)"
        [[ -n "$stale_pack" ]] || continue
        open_pids="$(lsof -nP -t "$pack" 2>/dev/null || true)"
        [[ -z "$open_pids" ]] || continue
        find "$pack" -type f -mmin "+$min_age_minutes" -delete
        removed_count=$((removed_count + 1))
    done

    if ((removed_count > 0)); then
        printf '[lux] removed %d stale Git temporary pack(s)\n' "$removed_count"
    fi
}
