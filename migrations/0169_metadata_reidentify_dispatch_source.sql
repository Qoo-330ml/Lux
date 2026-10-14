ALTER TABLE metadata_reidentify_jobs
    ADD COLUMN dispatch_source TEXT NOT NULL DEFAULT 'EXPLICIT'
    CHECK (dispatch_source IN ('EXPLICIT', 'AUTOMATIC'));

CREATE INDEX idx_metadata_reidentify_jobs_automatic_queue
    ON metadata_reidentify_jobs(library_id, created_at, id)
    WHERE mode = 'FILL_MISSING'
      AND status = 'QUEUED'
      AND cancel_requested = 0
      AND dispatch_source = 'AUTOMATIC';
