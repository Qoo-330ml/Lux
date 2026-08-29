CREATE TABLE emby_migration_handled_items (
    job_id TEXT NOT NULL REFERENCES emby_migration_jobs(id) ON DELETE CASCADE,
    emby_user_id TEXT NOT NULL,
    emby_item_id TEXT NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, emby_user_id, emby_item_id)
);
