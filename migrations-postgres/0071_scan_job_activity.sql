ALTER TABLE scan_jobs
    ADD COLUMN current_item TEXT;

ALTER TABLE scan_jobs
    ADD COLUMN scan_phase TEXT NOT NULL DEFAULT 'IDLE'
        CHECK (scan_phase IN ('DISCOVERY', 'INDEXING', 'FINALIZING', 'IDLE'));
