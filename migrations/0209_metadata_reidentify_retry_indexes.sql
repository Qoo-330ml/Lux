-- The scheduler checks due automatic metadata retries every 15 seconds. The
-- existing queue index only covers status = 'QUEUED'; this one matches the
-- retry query's DEFERRED / COMPLETED_WITH_ISSUES branch and its ordering.

CREATE INDEX idx_metadata_reidentify_jobs_automatic_retry
    ON metadata_reidentify_jobs(mode, dispatch_source, status, library_id)
    WHERE cancel_requested = 0 AND library_id IS NOT NULL;

CREATE INDEX idx_metadata_reidentify_items_automatic_retry
    ON metadata_reidentify_job_items(job_id)
    WHERE status = 'FAILED'
      AND error = 'SCRAPER_UNAVAILABLE'
      AND automatic_retry_consumed = 0
      AND request_fingerprint IS NOT NULL;
