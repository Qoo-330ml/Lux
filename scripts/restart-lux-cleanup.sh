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
