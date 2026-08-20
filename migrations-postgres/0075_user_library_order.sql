CREATE TABLE user_library_order (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    position BIGINT NOT NULL CHECK (position >= 0),
    PRIMARY KEY (user_id, library_id),
    UNIQUE (user_id, position)
);

CREATE INDEX idx_user_library_order_position
    ON user_library_order(user_id, position);
