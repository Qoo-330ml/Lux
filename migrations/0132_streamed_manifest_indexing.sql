ALTER TABLE scan_manifests
    ADD COLUMN workflow_version INTEGER NOT NULL DEFAULT 1
        CHECK (workflow_version IN (1, 2));

ALTER TABLE scan_manifest_roots
    ADD COLUMN next_observation_sequence INTEGER NOT NULL DEFAULT 0
        CHECK (next_observation_sequence >= 0);
