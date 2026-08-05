ALTER TABLE metadata_reidentify_jobs
    ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0
        CHECK (cancel_requested IN (0, 1));
