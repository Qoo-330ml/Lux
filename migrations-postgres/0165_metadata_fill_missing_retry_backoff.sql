ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN automatic_retry_count BIGINT NOT NULL DEFAULT 0
        CHECK (automatic_retry_count >= 0);

ALTER TABLE metadata_reidentify_job_items
    ADD COLUMN automatic_retry_after BIGINT;

INSERT INTO server_settings (key, value)
VALUES (
    'metadata_fill_missing_legacy_retry_after',
    (FLOOR(EXTRACT(EPOCH FROM CURRENT_TIMESTAMP))::BIGINT + 300)::TEXT
)
ON CONFLICT (key) DO NOTHING;
