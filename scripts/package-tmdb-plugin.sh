#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${TMDB_PLUGIN_VERSION:-0.1.5}"

case "$(uname -s)" in
  Darwin) PLATFORM="darwin" ;;
  Linux) PLATFORM="linux" ;;
  *) echo "unsupported host platform: $(uname -s)" >&2; exit 1 ;;
esac

case "$(uname -m)" in
  x86_64|amd64) ARCH="x86_64" ;;
  aarch64) ARCH="aarch64" ;;
  arm64) ARCH="arm64" ;;
  *) echo "unsupported host architecture: $(uname -m)" >&2; exit 1 ;;
esac

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"
cargo build --locked --release --bin lux-plugin-tmdb

OUTPUT="${1:-$ROOT_DIR/dist/org.lux.tmdb-${VERSION}.zip}"
mkdir -p "$(dirname "$OUTPUT")"

cargo run --locked --bin lux-plugin-pack -- \
  --binary "$TARGET_DIR/release/lux-plugin-tmdb" \
  --output "$OUTPUT" \
  --version "$VERSION" \
  --platform "$PLATFORM" \
  --arch "$ARCH"

echo "created $OUTPUT"
