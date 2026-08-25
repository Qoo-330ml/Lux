ALTER TABLE scan_job_targets
    DROP CONSTRAINT IF EXISTS scan_job_targets_change_kind_check;

ALTER TABLE scan_job_targets
    ADD CONSTRAINT scan_job_targets_change_kind_check
    CHECK (change_kind IN ('NEW', 'CHANGED', 'SIDECAR', 'REMOVED'));
