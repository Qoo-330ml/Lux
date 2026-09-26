ALTER TABLE filesystem_entries
    ADD COLUMN last_seen_change_kind TEXT
        CHECK (last_seen_change_kind IS NULL OR last_seen_change_kind IN ('NEW', 'CHANGED', 'SIDECAR'));

ALTER TABLE scan_manifests
    ADD COLUMN postprocessing_targets_ready BIGINT NOT NULL DEFAULT 1
        CHECK (postprocessing_targets_ready IN (0, 1));

ALTER TABLE scan_manifest_roots
    ADD COLUMN postprocessing_target_stage TEXT NOT NULL DEFAULT 'DONE'
        CHECK (postprocessing_target_stage IN ('NEW', 'CHANGED', 'DONE'));
ALTER TABLE scan_manifest_roots
    ADD COLUMN postprocessing_target_cursor TEXT;
