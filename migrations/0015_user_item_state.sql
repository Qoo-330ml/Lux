CREATE TABLE user_item_state (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    position_ticks INTEGER NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    is_played INTEGER NOT NULL DEFAULT 0 CHECK (is_played IN (0, 1)),
    is_favorite INTEGER NOT NULL DEFAULT 0 CHECK (is_favorite IN (0, 1)),
    play_count INTEGER NOT NULL DEFAULT 0 CHECK (play_count >= 0),
    last_played_at INTEGER,
    version INTEGER NOT NULL DEFAULT 0 CHECK (version >= 0),
    PRIMARY KEY (user_id, item_id)
);

CREATE INDEX idx_user_item_state_next_up
ON user_item_state(user_id, is_played, position_ticks, last_played_at);
