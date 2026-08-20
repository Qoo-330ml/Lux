ALTER TABLE metadata_reidentify_jobs
    DROP CONSTRAINT IF EXISTS metadata_reidentify_jobs_status_check;

ALTER TABLE metadata_reidentify_jobs
    ADD CONSTRAINT metadata_reidentify_jobs_status_check
    CHECK (status IN (
        'QUEUED', 'RUNNING', 'COMPLETED', 'COMPLETED_WITH_ISSUES', 'DEFERRED',
        'FAILED', 'CANCELLED'
    ));
