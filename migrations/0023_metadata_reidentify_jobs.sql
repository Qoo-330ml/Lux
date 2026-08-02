CREATE TABLE metadata_reidentify_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('QUEUED', 'RUNNING', 'COMPLETED', 'FAILED', 'CANCELLED')),
    processed_count INTEGER NOT NULL DEFAULT 0,
    total_count INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    started_at INTEGER,
    finished_at INTEGER
);

CREATE TABLE metadata_reidentify_job_items (
    job_id TEXT NOT NULL REFERENCES metadata_reidentify_jobs(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'FAILED')),
    candidate_count INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, item_id)
);

CREATE INDEX idx_metadata_reidentify_items_status
    ON metadata_reidentify_job_items(job_id, status, item_id);
