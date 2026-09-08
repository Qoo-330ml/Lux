DROP INDEX IF EXISTS idx_reconciliation_scan_entries_pending;

CREATE UNIQUE INDEX reconciliation_scan_entries_pk_reordered
    ON reconciliation_scan_entries(job_id, entry_type, library_root_id, relative_path);

ALTER TABLE reconciliation_scan_entries
    DROP CONSTRAINT IF EXISTS reconciliation_scan_entries_pkey;

ALTER TABLE reconciliation_scan_entries
    ADD CONSTRAINT reconciliation_scan_entries_pkey
    PRIMARY KEY USING INDEX reconciliation_scan_entries_pk_reordered;

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
