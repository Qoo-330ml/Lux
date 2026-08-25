-- no-transaction
PRAGMA foreign_keys = OFF;

CREATE TABLE scan_job_targets_new (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    target_type TEXT NOT NULL CHECK (target_type IN ('SOURCE', 'ITEM')),
    target_id TEXT NOT NULL,
    source_id TEXT,
    item_id TEXT NOT NULL,
    change_kind TEXT NOT NULL CHECK (change_kind IN ('NEW', 'CHANGED', 'SIDECAR', 'REMOVED')),
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

INSERT INTO scan_job_targets_new (
    job_id, target_type, target_id, source_id, item_id, change_kind,
    probe_state, metadata_state, thumbnail_state, error, created_at, updated_at
)
SELECT
    job_id, target_type, target_id, source_id, item_id, change_kind,
    probe_state, metadata_state, thumbnail_state, error, created_at, updated_at
FROM scan_job_targets;

DROP TABLE scan_job_targets;
ALTER TABLE scan_job_targets_new RENAME TO scan_job_targets;

CREATE INDEX idx_scan_job_targets_probe
    ON scan_job_targets(job_id, target_type, probe_state, target_id);

CREATE INDEX idx_scan_job_targets_metadata
    ON scan_job_targets(job_id, target_type, metadata_state, target_id);

CREATE INDEX idx_scan_job_targets_thumbnail
    ON scan_job_targets(job_id, target_type, thumbnail_state, target_id);

PRAGMA foreign_keys = ON;
