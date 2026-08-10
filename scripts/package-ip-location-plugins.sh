#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${IP_LOCATION_PLUGIN_VERSION:-0.1.0}"

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
OUTPUT_DIR="${1:-$ROOT_DIR/dist}"
mkdir -p "$OUTPUT_DIR"

cargo build --locked --release \
  --bin lux-plugin-ip-hiofd \
  --bin lux-plugin-qoo-ip138

cargo run --locked --bin lux-plugin-pack -- \
  --plugin ip-hiofd \
  --binary "$TARGET_DIR/release/lux-plugin-ip-hiofd" \
  --output "$OUTPUT_DIR/org.lux.ip-hiofd-${VERSION}.zip" \
  --version "$VERSION" \
  --platform "$PLATFORM" \
  --arch "$ARCH"

cargo run --locked --bin lux-plugin-pack -- \
  --plugin qoo-ip138 \
  --binary "$TARGET_DIR/release/lux-plugin-qoo-ip138" \
  --output "$OUTPUT_DIR/org.lux.qoo-ip138-${VERSION}.zip" \
  --version "$VERSION" \
  --platform "$PLATFORM" \
  --arch "$ARCH"

echo "created $OUTPUT_DIR/org.lux.ip-hiofd-${VERSION}.zip"
echo "created $OUTPUT_DIR/org.lux.qoo-ip138-${VERSION}.zip"
