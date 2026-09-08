-- no-transaction
PRAGMA foreign_keys = OFF;

DROP INDEX IF EXISTS idx_reconciliation_scan_entries_pending;

CREATE TABLE reconciliation_scan_entries_new (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    entry_type TEXT NOT NULL CHECK (entry_type IN ('DIRECTORY', 'FILE')),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, entry_type, library_root_id, relative_path)
);

INSERT INTO reconciliation_scan_entries_new (
    job_id, library_root_id, relative_path, entry_type, created_at
)
SELECT job_id, library_root_id, relative_path, entry_type, created_at
FROM reconciliation_scan_entries;

DROP TABLE reconciliation_scan_entries;
ALTER TABLE reconciliation_scan_entries_new RENAME TO reconciliation_scan_entries;

DROP INDEX IF EXISTS idx_scan_job_targets_probe;
CREATE INDEX idx_scan_job_targets_probe
    ON scan_job_targets(job_id, target_type, probe_state, target_id)
    WHERE probe_state IN ('PENDING', 'FAILED');

DROP INDEX IF EXISTS idx_scan_job_targets_metadata;
CREATE INDEX idx_scan_job_targets_metadata
    ON scan_job_targets(job_id, target_type, metadata_state, target_id)
    WHERE metadata_state IN ('PENDING', 'FAILED');

DROP INDEX IF EXISTS idx_scan_job_targets_thumbnail;
CREATE INDEX idx_scan_job_targets_thumbnail
    ON scan_job_targets(job_id, target_type, thumbnail_state, target_id)
    WHERE thumbnail_state IN ('PENDING', 'FAILED');

DROP INDEX IF EXISTS idx_media_streams_external_path;

PRAGMA foreign_keys = ON;
