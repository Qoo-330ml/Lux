-- pipeline_activity also reads DONE targets. The existing PENDING/FAILED
-- partial indexes stay for unfinished-stage diagnostics; add DONE-only indexes
-- so the pipeline query can combine them instead of scanning the whole table.
CREATE INDEX IF NOT EXISTS idx_scan_job_targets_metadata_done
    ON scan_job_targets(job_id, target_type, target_id)
    WHERE metadata_state = 'DONE';

CREATE INDEX IF NOT EXISTS idx_scan_job_targets_probe_done
    ON scan_job_targets(job_id, target_type, target_id)
    WHERE probe_state = 'DONE';

CREATE INDEX IF NOT EXISTS idx_scan_job_targets_thumbnail_done
    ON scan_job_targets(job_id, target_type, target_id)
    WHERE thumbnail_state = 'DONE';
