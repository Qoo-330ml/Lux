CREATE TABLE user_playback_settings (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    played_percent INTEGER NOT NULL DEFAULT 95 CHECK (played_percent BETWEEN 1 AND 100),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
