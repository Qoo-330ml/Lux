#!/bin/sh
set -eu

config_dir="${LUX_CONFIG_DIR:-/config}"
mkdir -p "$config_dir"

# Bind mounts are commonly created as root on the host. Hand over the config
# directory and its immediate files, without recursively changing a media tree
# or following symlinks supplied through the mount.
if [ -L "$config_dir" ]; then
    echo "refusing symlinked config directory: $config_dir" >&2
    exit 1
fi
chown -h lux:lux "$config_dir"
for entry in "$config_dir"/* "$config_dir"/.[!.]* "$config_dir"/..?*; do
    if [ -e "$entry" ] || [ -L "$entry" ]; then
        chown -h lux:lux "$entry"
    fi
done

exec runuser --user lux -- "$@"
