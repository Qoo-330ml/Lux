ALTER TABLE strm_probe_jobs
ADD COLUMN target_scan_job_id TEXT REFERENCES scan_jobs(id) ON DELETE SET NULL;

CREATE INDEX idx_strm_probe_jobs_target_scan_job
    ON strm_probe_jobs(target_scan_job_id);
