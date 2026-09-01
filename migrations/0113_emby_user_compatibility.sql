ALTER TABLE users
ADD COLUMN has_password INTEGER NOT NULL DEFAULT 1 CHECK (has_password IN (0, 1));

CREATE TABLE user_emby_configuration (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    configuration_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
