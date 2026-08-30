DROP INDEX IF EXISTS idx_scan_jobs_one_active;

CREATE UNIQUE INDEX idx_scan_jobs_one_active
    ON scan_jobs(library_id, job_type)
    WHERE status IN ('PENDING', 'RUNNING');
