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

# The bundled TMDb plugin must live in the persistent plugin directory because
# a bind mount at /config hides any image content below that path.
builtin_tmdb_plugin="/usr/local/share/lux/plugins/org.lux.tmdb.zip"
if [ -f "$builtin_tmdb_plugin" ] \
    && ! find "$plugin_dir" -maxdepth 1 -type f -name 'org.lux.tmdb*.zip' -print -quit | grep -q . \
    && [ ! -f "$plugin_dir/org.lux.tmdb/manifest.json" ]; then
    cp "$builtin_tmdb_plugin" "$plugin_dir/org.lux.tmdb.zip"
fi

# Lux runs as root so bind-mounted NAS directories remain usable regardless of
# the host-side UID/GID. Do not rewrite ownership of user data at startup.
exec "$@"
