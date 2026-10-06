CREATE INDEX idx_scan_jobs_failed_count
    ON scan_jobs(id)
    WHERE status = 'FAILED';
