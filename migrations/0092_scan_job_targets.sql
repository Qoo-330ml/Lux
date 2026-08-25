CREATE TABLE scan_job_targets (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    target_type TEXT NOT NULL CHECK (target_type IN ('SOURCE', 'ITEM')),
    target_id TEXT NOT NULL,
    source_id TEXT,
    item_id TEXT NOT NULL,
    change_kind TEXT NOT NULL CHECK (change_kind IN ('NEW', 'CHANGED', 'SIDECAR')),
    probe_state TEXT NOT NULL DEFAULT 'SKIPPED'
        CHECK (probe_state IN ('PENDING', 'DONE', 'FAILED', 'SKIPPED')),
    metadata_state TEXT NOT NULL DEFAULT 'SKIPPED'
        CHECK (metadata_state IN ('PENDING', 'DONE', 'FAILED', 'SKIPPED')),
    thumbnail_state TEXT NOT NULL DEFAULT 'SKIPPED'
        CHECK (thumbnail_state IN ('PENDING', 'DONE', 'FAILED', 'SKIPPED')),
    error TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, target_type, target_id)
);

CREATE INDEX idx_scan_job_targets_probe
    ON scan_job_targets(job_id, target_type, probe_state, target_id);

CREATE INDEX idx_scan_job_targets_metadata
    ON scan_job_targets(job_id, target_type, metadata_state, target_id);

CREATE INDEX idx_scan_job_targets_thumbnail
    ON scan_job_targets(job_id, target_type, thumbnail_state, target_id);
