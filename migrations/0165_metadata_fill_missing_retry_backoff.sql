ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN automatic_retry_count INTEGER NOT NULL DEFAULT 0
        CHECK (automatic_retry_count >= 0);

ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN automatic_retry_after INTEGER;

ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN automatic_retry_consumed INTEGER NOT NULL DEFAULT 0
        CHECK (automatic_retry_consumed IN (0, 1));

INSERT INTO server_settings (key, value)
VALUES ('metadata_fill_missing_legacy_retry_after', CAST(unixepoch() + 300 AS TEXT))
ON CONFLICT(key) DO NOTHING;
