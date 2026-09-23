CREATE TABLE login_background_plugin_cache (
    plugin_id TEXT PRIMARY KEY NOT NULL
        REFERENCES installed_plugins(plugin_id) ON DELETE CASCADE,
    payload_json TEXT NOT NULL
        CHECK (length(CAST(payload_json AS BLOB)) <= 262144),
    refreshed_at INTEGER NOT NULL CHECK (refreshed_at > 0)
);
