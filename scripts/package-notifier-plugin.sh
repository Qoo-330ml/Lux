#!/usr/bin/env bash
# Packages one notification plugin directory from `plugins/<name>` into
# `dist/plugins/<id>-<version>-<platform>-<arch>.zip`.
#
# The package is derived from the manifest instead of hard-coded names: the plugin ID, version,
# entrypoint and declared files come from `plugins/<name>/manifest.json`, and the cargo binary name
# is the entrypoint file name. Only a `notification` plugin declaring `notification.send` may be
# packaged, so this script cannot silently ship a different plugin type.
#
# Usage: scripts/package-notifier-plugin.sh <plugin directory name> [rust target]
set -euo pipefail

project_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

usage() {
  cat >&2 <<'USAGE'
usage: package-notifier-plugin.sh <plugin directory name> [rust target]

example:
  scripts/package-notifier-plugin.sh org.lux.webhook
  scripts/package-notifier-plugin.sh org.lux.webhook aarch64-apple-darwin
USAGE
}

if [[ $# -lt 1 || -z "${1:-}" ]]; then
  usage
  exit 2
fi

plugin_dir_name=$1
target=${2:-}
plugin_dir="$project_root/plugins/$plugin_dir_name"
manifest="$plugin_dir/manifest.json"

if [[ ! -f "$manifest" ]]; then
  echo "notification plugin manifest is missing: $manifest" >&2
  exit 1
fi

# Read the packaging plan straight from the manifest: one field per line, followed by the declared
# package files (none today).
manifest_fields=$(python3 - "$manifest" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    manifest = json.load(handle)

runtime = manifest.get("runtime") or {}
fields = [
    str(manifest.get("id", "") or ""),
    str(manifest.get("version", "") or ""),
    str(manifest.get("type", "") or ""),
    ",".join(str(item) for item in manifest.get("capabilities") or []),
    str(runtime.get("entrypoint", "") or ""),
]
for entry in manifest.get("files") or []:
    fields.append(str(entry.get("path", "") or ""))
sys.stdout.write("\n".join(fields) + "\n")
PY
)

plugin_id=$(sed -n '1p' <<<"$manifest_fields")
manifest_version=$(sed -n '2p' <<<"$manifest_fields")
plugin_type=$(sed -n '3p' <<<"$manifest_fields")
plugin_capabilities=$(sed -n '4p' <<<"$manifest_fields")
entrypoint_template=$(sed -n '5p' <<<"$manifest_fields")
declared_files=$(sed -n '6,$p' <<<"$manifest_fields" | sed '/^$/d')

if [[ -z "$plugin_id" || -z "$manifest_version" || -z "$entrypoint_template" ]]; then
  echo "manifest is incomplete: id, version and runtime.entrypoint are required" >&2
  exit 1
fi
if [[ "$plugin_type" != "notification" ]]; then
  echo "$plugin_id is a '$plugin_type' plugin, not a notification plugin" >&2
  exit 1
fi
if [[ ",$plugin_capabilities," != *",notification.send,"* ]]; then
  echo "$plugin_id does not declare the notification.send capability" >&2
  exit 1
fi

# The package version always comes from the manifest: an environment override could ship a
# version that contradicts the manifest the host validates after installation.
version=$manifest_version

if [[ -z "$target" ]]; then
  target=$(rustc -vV | sed -n 's/^host: //p')
fi

case "$target" in
  *-apple-darwin) platform=darwin ;;
  *-unknown-linux-gnu | *-unknown-linux-musl) platform=linux ;;
  *-pc-windows-gnu | *-pc-windows-msvc) platform=windows ;;
  *)
    echo "unsupported Rust target: $target" >&2
    exit 1
    ;;
esac

arch=${target%%-*}
case "$target" in
  aarch64-apple-darwin) arch=arm64 ;;
esac

entrypoint=${entrypoint_template//\$\{platform\}/$platform}
entrypoint=${entrypoint//\$\{arch\}/$arch}
if [[ "$entrypoint" == /* || "$entrypoint" == *".."* ]]; then
  echo "manifest entrypoint must stay inside the package: $entrypoint_template" >&2
  exit 1
fi

binary_name=$(basename "$entrypoint")
cargo_bin=$binary_name
if [[ "$platform" == windows ]]; then
  if [[ "$binary_name" != *.exe ]]; then
    binary_name+=".exe"
  fi
  cargo_bin=${cargo_bin%.exe}
fi

cargo_args=(build --locked --release --bin "$cargo_bin")
if [[ -n "${2:-}" ]]; then
  cargo_args+=(--target "$target")
fi
(cd "$project_root" && cargo "${cargo_args[@]}")

binary_path="$project_root/target"
if [[ -n "${2:-}" ]]; then
  binary_path+="/$target"
fi
binary_path+="/release/$binary_name"
if [[ ! -f "$binary_path" ]]; then
  echo "plugin binary was not built: $binary_path" >&2
  exit 1
fi

dist_dir="$project_root/dist/plugins"
mkdir -p "$dist_dir"
staging_dir=$(mktemp -d "${TMPDIR:-/tmp}/lux-notifier-plugin.XXXXXX")
trap 'rm -rf "$staging_dir"' EXIT

mkdir -p "$staging_dir/$(dirname "$entrypoint")"
cp "$manifest" "$staging_dir/manifest.json"
cp "$binary_path" "$staging_dir/$entrypoint"
if [[ "$platform" != windows ]]; then
  chmod +x "$staging_dir/$entrypoint"
fi
while IFS= read -r declared_file; do
  [[ -z "$declared_file" ]] && continue
  if [[ "$declared_file" == /* || "$declared_file" == *".."* ]]; then
    echo "manifest file must stay inside the package: $declared_file" >&2
    exit 1
  fi
  if [[ ! -e "$plugin_dir/$declared_file" ]]; then
    echo "manifest file is missing from the plugin directory: $declared_file" >&2
    exit 1
  fi
  mkdir -p "$staging_dir/$(dirname "$declared_file")"
  cp -R "$plugin_dir/$declared_file" "$staging_dir/$declared_file"
done <<<"$declared_files"

archive="$dist_dir/$plugin_id-$version-$platform-$arch.zip"
rm -f "$archive"
(cd "$staging_dir" && zip -q -r "$archive" .)
echo "$archive"
