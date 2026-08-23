CREATE TABLE user_person_state (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    person_id TEXT NOT NULL,
    is_favorite INTEGER NOT NULL DEFAULT 0 CHECK (is_favorite IN (0, 1)),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (user_id, person_id)
);

CREATE INDEX idx_user_person_state_favorites
ON user_person_state(user_id, is_favorite, updated_at DESC, person_id);
