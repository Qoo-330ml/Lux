ALTER TABLE scan_jobs
    ADD COLUMN discovery_completed INTEGER NOT NULL DEFAULT 1
        CHECK (discovery_completed IN (0, 1));

CREATE TABLE reconciliation_scan_entries (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    entry_type TEXT NOT NULL CHECK (entry_type IN ('DIRECTORY', 'FILE')),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, library_root_id, entry_type, relative_path)
);

CREATE INDEX idx_reconciliation_scan_entries_pending
    ON reconciliation_scan_entries(job_id, entry_type, library_root_id, relative_path);

INSERT INTO reconciliation_scan_entries (
    job_id, library_root_id, relative_path, entry_type
)
SELECT sj.id, lr.id, '', 'DIRECTORY'
FROM scan_jobs sj
JOIN library_roots lr ON lr.library_id = sj.library_id
WHERE sj.job_type = 'RECONCILE_LIBRARY'
  AND sj.status IN ('PENDING', 'RUNNING');

UPDATE scan_jobs
SET discovery_completed = 0,
    processed_count = 0,
    total_count = 0,
    cursor = NULL,
    updated_at = unixepoch()
WHERE job_type = 'RECONCILE_LIBRARY'
  AND status IN ('PENDING', 'RUNNING');
