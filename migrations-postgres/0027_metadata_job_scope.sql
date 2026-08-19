ALTER TABLE metadata_reidentify_jobs
    ADD COLUMN job_scope TEXT NOT NULL DEFAULT 'ITEMS'
        CHECK (job_scope IN ('ITEMS', 'LIBRARY'));

UPDATE metadata_reidentify_jobs
SET job_scope = 'LIBRARY'
WHERE library_id IS NOT NULL AND total_count > 100;

CREATE INDEX idx_metadata_reidentify_jobs_active_scope
    ON metadata_reidentify_jobs(job_scope, status, created_at DESC, id DESC);
