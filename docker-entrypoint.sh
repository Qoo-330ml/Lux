#!/bin/sh
set -eu

config_dir="${LUX_CONFIG_DIR:-/config}"
if [ -L "$config_dir" ]; then
    echo "refusing symlinked config directory: $config_dir" >&2
    exit 1
fi
mkdir -p "$config_dir"

# Lux runs as root so bind-mounted NAS directories remain usable regardless of
# the host-side UID/GID. Do not rewrite ownership of user data at startup.
exec "$@"
