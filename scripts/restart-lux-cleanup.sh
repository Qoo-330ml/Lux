#!/usr/bin/env bash

cleanup_old_debug_artifacts() {
    local target_dir="${1:-}"
    local min_age_minutes="${2:-1440}"
    local debug_dir
    local cache_dir

    if [[ -z "$target_dir" || "$target_dir" == "/" || "$target_dir" == "." ]]; then
        printf '[lux] refusing to clean an unsafe target directory: %s\n' "$target_dir" >&2
        return 2
    fi

    if [[ ! "$min_age_minutes" =~ ^[1-9][0-9]*$ ]]; then
        printf '[lux] invalid cleanup age in minutes: %s\n' "$min_age_minutes" >&2
        return 2
    fi

    debug_dir="$target_dir/debug"
    [[ -d "$debug_dir" ]] || return 0

    for cache_dir in \
        "$debug_dir/deps" \
        "$debug_dir/incremental" \
        "$debug_dir/build" \
        "$debug_dir/.fingerprint"; do
        [[ -d "$cache_dir" ]] || continue
        find "$cache_dir" -type f -mmin "+$min_age_minutes" -delete
    done
}
