CREATE TABLE web_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    session_token_hash BLOB NOT NULL UNIQUE,
    csrf_token_hash BLOB NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    expires_at INTEGER NOT NULL,
    last_seen_at INTEGER,
    revoked_at INTEGER
);

CREATE INDEX web_sessions_user_id_idx ON web_sessions(user_id);
CREATE INDEX web_sessions_active_idx ON web_sessions(session_token_hash, expires_at, revoked_at);
