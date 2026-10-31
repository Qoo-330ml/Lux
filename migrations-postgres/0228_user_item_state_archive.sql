-- Watch state that outlives the media item it was recorded against. See the SQLite migration
-- 0226_user_item_state_archive.sql for the reasoning; the schema is the same, with BIGINT
-- integers as everywhere else in this directory.
CREATE TABLE user_item_state_archive (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_type TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    position_ticks BIGINT NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    is_played BIGINT NOT NULL DEFAULT 0 CHECK (is_played IN (0, 1)),
    is_favorite BIGINT NOT NULL DEFAULT 0 CHECK (is_favorite IN (0, 1)),
    play_count BIGINT NOT NULL DEFAULT 0 CHECK (play_count >= 0),
    last_played_at BIGINT,
    detached_at BIGINT NOT NULL DEFAULT (unixepoch()),
    source_item_id TEXT NOT NULL,
    PRIMARY KEY (user_id, item_type, provider, provider_id)
);

CREATE INDEX idx_user_item_state_archive_identity
    ON user_item_state_archive(item_type, provider, provider_id);
