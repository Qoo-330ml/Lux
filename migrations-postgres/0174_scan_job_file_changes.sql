CREATE TABLE scan_job_file_changes (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    change_kind TEXT NOT NULL CHECK (change_kind IN ('ADD', 'CHANGE', 'REMOVE')),
    processing_state TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (processing_state IN ('PENDING', 'RUNNING', 'DONE', 'FAILED', 'SKIPPED')),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    updated_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    PRIMARY KEY (job_id, library_root_id, relative_path)
);

CREATE INDEX idx_scan_job_file_changes_pending
    ON scan_job_file_changes(job_id, processing_state, relative_path, library_root_id);
