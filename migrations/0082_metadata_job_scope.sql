ALTER TABLE metadata_reidentify_jobs
    ADD COLUMN library_id TEXT REFERENCES libraries(id) ON DELETE SET NULL;

ALTER TABLE metadata_reidentify_jobs
    ADD COLUMN job_scope TEXT NOT NULL DEFAULT 'ITEMS'
        CHECK (job_scope IN ('ITEMS', 'LIBRARY'));

UPDATE metadata_reidentify_jobs AS jobs
SET library_id = (
    SELECT MIN(media_items.library_id)
    FROM metadata_reidentify_job_items
    JOIN media_items ON media_items.id = metadata_reidentify_job_items.item_id
    WHERE metadata_reidentify_job_items.job_id = jobs.id
    GROUP BY metadata_reidentify_job_items.job_id
    HAVING COUNT(DISTINCT media_items.library_id) = 1
);

CREATE INDEX idx_metadata_reidentify_items_item_job
    ON metadata_reidentify_job_items(item_id, job_id);

CREATE INDEX idx_metadata_reidentify_jobs_scope_status
    ON metadata_reidentify_jobs(library_id, job_scope, status, created_at DESC, id DESC);
