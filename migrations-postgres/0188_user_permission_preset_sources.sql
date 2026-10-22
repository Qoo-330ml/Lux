CREATE TABLE user_permission_preset_sources (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    preset_id TEXT NOT NULL CHECK (length(trim(preset_id)) > 0),
    preset_name TEXT NOT NULL CHECK (length(trim(preset_name)) > 0),
    preset_version BIGINT NOT NULL CHECK (preset_version > 0),
    applied_at BIGINT NOT NULL DEFAULT (unixepoch())
);
