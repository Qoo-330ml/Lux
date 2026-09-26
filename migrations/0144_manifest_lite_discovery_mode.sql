ALTER TABLE scan_manifests
    ADD COLUMN discovery_mode TEXT NOT NULL DEFAULT 'PERSISTED'
        CHECK (discovery_mode IN ('PERSISTED', 'LITE'));
