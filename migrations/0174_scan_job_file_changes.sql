CREATE TABLE scan_job_file_changes (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    change_kind TEXT NOT NULL CHECK (change_kind IN ('ADD', 'CHANGE', 'REMOVE')),
    processing_state TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (processing_state IN ('PENDING', 'RUNNING', 'DONE', 'FAILED', 'SKIPPED')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    error TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, library_root_id, relative_path)
);

CREATE INDEX idx_scan_job_file_changes_pending
    ON scan_job_file_changes(job_id, processing_state, relative_path, library_root_id);
