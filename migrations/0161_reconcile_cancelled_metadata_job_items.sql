-- Cancelled metadata jobs must not leave item rows looking claimable.
-- FAILED is the existing retryable terminal item state; retry_metadata_reidentify_job
-- moves it back to PENDING when an administrator explicitly retries the job.
UPDATE metadata_reidentify_job_items
SET status = 'FAILED',
    candidate_count = 0,
    error = 'JOB_CANCELLED',
    updated_at = unixepoch()
WHERE status IN ('PENDING', 'RUNNING')
  AND EXISTS (
      SELECT 1
      FROM metadata_reidentify_jobs jobs
      WHERE jobs.id = metadata_reidentify_job_items.job_id
        AND jobs.status = 'CANCELLED'
  );
