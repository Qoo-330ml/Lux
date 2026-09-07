#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
extension_root="$repo_root/tools/chrome-caption-extension"
dist_dir="$extension_root/dist"
artifact_dir="$repo_root/output"
artifact_path="${1:-$artifact_dir/lux-remote-matroska-media-v0.4.0.zip}"

mkdir -p "$artifact_dir"
pnpm --dir "$repo_root/web" exec vite build --config ../tools/chrome-caption-extension/vite.config.ts

test -f "$dist_dir/manifest.json"
test -f "$dist_dir/content-script.js"
test -f "$dist_dir/service-worker.js"

artifact_path="$(python3 -c 'import os,sys; print(os.path.abspath(sys.argv[1]))' "$artifact_path")"
rm -f "$artifact_path"
(cd "$dist_dir" && zip -q -r "$artifact_path" manifest.json content-script.js service-worker.js)
unzip -tq "$artifact_path" >/dev/null
printf '%s\n' "$artifact_path"
