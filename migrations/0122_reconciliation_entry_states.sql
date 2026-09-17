ALTER TABLE reconciliation_scan_entries
    ADD COLUMN status TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING', 'DONE'));

CREATE INDEX idx_reconciliation_scan_entries_pending_state
    ON reconciliation_scan_entries(job_id, entry_type, library_root_id, relative_path)
    WHERE status = 'PENDING';
