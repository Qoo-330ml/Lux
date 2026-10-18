ALTER TABLE scan_jobs
    ADD COLUMN processed_media_count INTEGER DEFAULT 0
    CHECK (processed_media_count IS NULL OR processed_media_count >= 0);

UPDATE scan_jobs SET processed_media_count = NULL;

CREATE TABLE scan_job_processed_media_items (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    media_item_id TEXT NOT NULL,
    PRIMARY KEY (job_id, media_item_id)
);
