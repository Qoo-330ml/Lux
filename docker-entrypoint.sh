#!/bin/sh
set -eu

config_dir="${LUX_CONFIG_DIR:-/config}"
if [ -L "$config_dir" ]; then
    echo "refusing symlinked config directory: $config_dir" >&2
    exit 1
fi
mkdir -p "$config_dir"
plugin_dir="$config_dir/plugins"
mkdir -p "$plugin_dir"

# Bundled plugins must live in the persistent plugin directory because a bind
# mount at /config hides any image content below that path.
for plugin_id in org.lux.tmdb org.lux.ip-hiofd org.lux.qoo-ip138; do
    bundled_plugin="/usr/local/share/lux/plugins/$plugin_id.zip"
    if [ -f "$bundled_plugin" ] \
        && ! find "$plugin_dir" -maxdepth 1 -type f -name "$plugin_id*.zip" -print -quit | grep -q . \
        && [ ! -f "$plugin_dir/$plugin_id/manifest.json" ]; then
        cp "$bundled_plugin" "$plugin_dir/$plugin_id.zip"
    fi
done

# Lux runs as root so bind-mounted NAS directories remain usable regardless of
# the host-side UID/GID. Do not rewrite ownership of user data at startup.
exec "$@"
