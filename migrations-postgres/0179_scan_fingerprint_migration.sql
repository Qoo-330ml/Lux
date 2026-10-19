CREATE TABLE scan_fingerprint_migration_state (
    library_id TEXT PRIMARY KEY NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    cursor_root_id TEXT,
    cursor_relative_path TEXT,
    lease_token TEXT,
    lease_expires_at BIGINT,
    completed_at BIGINT,
    updated_at BIGINT NOT NULL DEFAULT (extract(epoch FROM now())::BIGINT)
);

CREATE TABLE scan_fingerprint_migration_deferred (
    filesystem_entry_id TEXT PRIMARY KEY NOT NULL
        REFERENCES filesystem_entries(id) ON DELETE CASCADE,
    attempts BIGINT NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    retry_at BIGINT NOT NULL,
    reason TEXT NOT NULL,
    updated_at BIGINT NOT NULL DEFAULT (extract(epoch FROM now())::BIGINT)
);

CREATE TABLE scan_fingerprint_migration_deferred_roots (
    library_root_id TEXT PRIMARY KEY NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    retry_at BIGINT NOT NULL,
    reason TEXT NOT NULL,
    updated_at BIGINT NOT NULL DEFAULT (extract(epoch FROM now())::BIGINT)
);

CREATE INDEX idx_filesystem_entries_scan_fingerprint_legacy
    ON filesystem_entries(library_root_id, relative_path)
    WHERE entry_kind = 'FILE' AND is_missing = 0 AND length(fingerprint) = 32;

CREATE INDEX idx_filesystem_entries_scan_fingerprint_stat_lookup
    ON filesystem_entries(size, modified_at, relative_path)
    WHERE entry_kind = 'FILE' AND is_missing = 0 AND length(fingerprint) = 25;
