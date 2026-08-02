CREATE TABLE access_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    token_hash BLOB NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    client_name TEXT NOT NULL,
    device_name TEXT NOT NULL,
    client_version TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_seen_at INTEGER,
    revoked_at INTEGER
);

CREATE INDEX access_tokens_user_id_idx ON access_tokens(user_id);
CREATE INDEX access_tokens_active_idx ON access_tokens(token_hash, revoked_at);
