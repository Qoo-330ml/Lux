CREATE TABLE user_collection_source_preferences (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    work_item_id TEXT NOT NULL,
    source_item_id TEXT NOT NULL,
    media_source_id TEXT REFERENCES media_sources(id) ON DELETE SET NULL,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (user_id, work_item_id),
    FOREIGN KEY (work_item_id, source_item_id)
        REFERENCES collection_work_sources(work_item_id, source_item_id)
        ON DELETE CASCADE
);

CREATE INDEX idx_user_collection_source_preferences_work
    ON user_collection_source_preferences(work_item_id, user_id);
