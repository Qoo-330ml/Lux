CREATE TABLE library_root_history (
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    canonical_path TEXT NOT NULL,
    root_id TEXT NOT NULL,
    deleted_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (library_id, canonical_path),
    UNIQUE (root_id)
);

CREATE INDEX idx_library_root_history_root_id
    ON library_root_history(root_id);
