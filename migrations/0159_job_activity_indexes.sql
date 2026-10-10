CREATE INDEX IF NOT EXISTS idx_scan_jobs_activity
    ON scan_jobs(status, scan_phase, created_at DESC, id DESC)
    WHERE status IN ('PENDING', 'RUNNING')
       OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING');

CREATE INDEX IF NOT EXISTS idx_metadata_reidentify_jobs_activity
    ON metadata_reidentify_jobs(status, created_at DESC, id DESC)
    WHERE status IN ('QUEUED', 'RUNNING');

CREATE INDEX IF NOT EXISTS idx_metadata_reidentify_jobs_recent
    ON metadata_reidentify_jobs(created_at DESC, id DESC);
