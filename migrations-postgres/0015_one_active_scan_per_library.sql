UPDATE scan_jobs
SET status = 'CANCELLED',
    cancel_requested = 1,
    error = 'cancelled because another scan for this library is active',
    updated_at = unixepoch(),
    finished_at = unixepoch()
WHERE status IN ('PENDING', 'RUNNING')
  AND EXISTS (
      SELECT 1
      FROM scan_jobs newer
      WHERE newer.library_id = scan_jobs.library_id
        AND newer.status IN ('PENDING', 'RUNNING')
        AND (newer.created_at > scan_jobs.created_at
             OR (newer.created_at = scan_jobs.created_at AND newer.id > scan_jobs.id))
  );

DELETE FROM reconciliation_scan_entries
WHERE job_id IN (SELECT id FROM scan_jobs WHERE status = 'CANCELLED');

DELETE FROM scan_job_paths
WHERE job_id IN (SELECT id FROM scan_jobs WHERE status = 'CANCELLED');

DROP INDEX idx_scan_jobs_one_active;

CREATE UNIQUE INDEX idx_scan_jobs_one_active
    ON scan_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');
