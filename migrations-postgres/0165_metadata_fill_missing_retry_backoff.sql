ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN automatic_retry_count BIGINT NOT NULL DEFAULT 0
        CHECK (automatic_retry_count >= 0);

ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN automatic_retry_after BIGINT;

UPDATE metadata_reidentify_job_items
SET automatic_retry_count = 1,
    automatic_retry_after = FLOOR(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP))::BIGINT + 300
WHERE status = 'FAILED'
  AND error = 'SCRAPER_UNAVAILABLE'
  AND request_fingerprint IS NOT NULL
  AND EXISTS (
      SELECT 1
      FROM metadata_reidentify_jobs jobs
      WHERE jobs.id = metadata_reidentify_job_items.job_id
        AND jobs.mode = 'FILL_MISSING'
        AND jobs.status = 'DEFERRED'
  );
