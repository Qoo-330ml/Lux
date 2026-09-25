ALTER TABLE scan_manifests
    ADD COLUMN discovery_format_version BIGINT NOT NULL DEFAULT 2
        CHECK (discovery_format_version IN (2, 3));

CREATE TABLE scan_manifest_seen_paths (
    manifest_id TEXT NOT NULL,
    library_root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    PRIMARY KEY (manifest_id, library_root_id, relative_path),
    FOREIGN KEY (manifest_id, library_root_id)
        REFERENCES scan_manifest_roots(manifest_id, library_root_id) ON DELETE CASCADE
);
