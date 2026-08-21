#!/usr/bin/env bash
set -euo pipefail

project_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target=${1:-}
version=${LUX_DANMAKU_PLUGIN_VERSION:-1.0.0}

if [[ -z "$target" ]]; then
  target=$(rustc -vV | sed -n 's/^host: //p')
fi

case "$target" in
  *-apple-darwin) platform=darwin ;;
  *-unknown-linux-gnu|*-unknown-linux-musl) platform=linux ;;
  *-pc-windows-gnu|*-pc-windows-msvc) platform=windows ;;
  *) echo "unsupported Rust target: $target" >&2; exit 1 ;;
esac

arch=${target%%-*}
case "$target" in
  aarch64-apple-darwin) arch=arm64 ;;
esac

binary_name=lux-plugin-danmaku
if [[ "$platform" == windows ]]; then binary_name+=".exe"; fi

cargo_args=(build --locked --release --bin lux-plugin-danmaku)
if [[ -n "${1:-}" ]]; then cargo_args+=(--target "$target"); fi
(cd "$project_root" && cargo "${cargo_args[@]}")

binary_path="$project_root/target"
if [[ -n "${1:-}" ]]; then binary_path+="/$target"; fi
binary_path+="/release/$binary_name"
if [[ ! -f "$binary_path" ]]; then
  echo "plugin binary was not built: $binary_path" >&2
  exit 1
fi

dist_dir="$project_root/dist/plugins"
mkdir -p "$dist_dir"
staging_dir=$(mktemp -d "${TMPDIR:-/tmp}/lux-danmaku-plugin.XXXXXX")
trap 'rm -rf "$staging_dir"' EXIT
mkdir -p "$staging_dir/binaries/$platform-$arch"
cp "$project_root/plugins/org.lux.danmaku/manifest.json" "$staging_dir/manifest.json"
cp "$binary_path" "$staging_dir/binaries/$platform-$arch/$binary_name"

archive="$dist_dir/org.lux.danmaku-$version-$platform-$arch.zip"
rm -f "$archive"
(cd "$staging_dir" && zip -q -r "$archive" manifest.json "binaries/$platform-$arch/$binary_name")
echo "$archive"
