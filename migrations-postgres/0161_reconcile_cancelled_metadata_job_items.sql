-- Cancelled metadata jobs must not leave item rows looking claimable.
UPDATE metadata_reidentify_job_items
SET status = 'FAILED',
    candidate_count = 0,
    error = 'JOB_CANCELLED',
    updated_at = CAST(EXTRACT(EPOCH FROM NOW()) AS BIGINT)
WHERE status IN ('PENDING', 'RUNNING')
  AND EXISTS (
      SELECT 1
      FROM metadata_reidentify_jobs jobs
      WHERE jobs.id = metadata_reidentify_job_items.job_id
        AND jobs.status = 'CANCELLED'
  );
