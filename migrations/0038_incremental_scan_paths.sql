CREATE TABLE scan_job_paths (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    change_kind TEXT NOT NULL CHECK (change_kind IN ('CREATE', 'MODIFY', 'RENAME', 'REMOVE')),
    processed_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, library_root_id, relative_path)
);

CREATE INDEX idx_scan_job_paths_pending
    ON scan_job_paths(job_id, processed_at, created_at, relative_path);

UPDATE libraries SET realtime_watch_enabled = 1 WHERE realtime_watch_enabled = 0;
