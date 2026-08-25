ALTER TABLE scan_jobs
    DROP CONSTRAINT IF EXISTS scan_jobs_scan_phase_check;

ALTER TABLE scan_jobs
    ADD CONSTRAINT scan_jobs_scan_phase_check
    CHECK (scan_phase IN ('DISCOVERY', 'INDEXING', 'FINALIZING', 'POSTPROCESSING', 'IDLE'));
