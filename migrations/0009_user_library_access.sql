CREATE TABLE user_library_access (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    can_view INTEGER NOT NULL DEFAULT 0 CHECK (can_view IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (user_id, library_id)
);

CREATE INDEX idx_user_library_access_library_id ON user_library_access(library_id, can_view);
