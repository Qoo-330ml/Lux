ALTER TABLE access_tokens ADD COLUMN device_type TEXT;

CREATE TABLE device_pairings (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    secret_hash BYTEA NOT NULL UNIQUE,
    expires_at BIGINT NOT NULL,
    consumed_at BIGINT,
    cancelled_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX device_pairings_user_id_idx ON device_pairings(user_id);
CREATE INDEX device_pairings_active_idx
    ON device_pairings(user_id, expires_at)
    WHERE consumed_at IS NULL AND cancelled_at IS NULL;
