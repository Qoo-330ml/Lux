ALTER TABLE metadata_reidentify_jobs
    ADD COLUMN library_id TEXT REFERENCES libraries(id) ON DELETE SET NULL;

UPDATE metadata_reidentify_jobs jobs
SET library_id = summaries.library_id
FROM (
    SELECT job_items.job_id,
           CASE
               WHEN MIN(media_items.library_id) = MAX(media_items.library_id)
                   THEN MIN(media_items.library_id)
               ELSE NULL
           END AS library_id
    FROM metadata_reidentify_job_items job_items
    JOIN media_items ON media_items.id = job_items.item_id
    GROUP BY job_items.job_id
) summaries
WHERE summaries.job_id = jobs.id;

CREATE INDEX idx_metadata_reidentify_items_item_job
    ON metadata_reidentify_job_items(item_id, job_id);

CREATE INDEX idx_metadata_reidentify_jobs_created
    ON metadata_reidentify_jobs(created_at DESC, id DESC);
