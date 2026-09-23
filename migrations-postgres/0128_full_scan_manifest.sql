CREATE TABLE scan_manifests (
    id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL UNIQUE REFERENCES scan_jobs(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN (
        'DISCOVERING', 'READY_TO_DIFF', 'APPLYING', 'INDEXED',
        'POSTPROCESSING', 'COMPLETED', 'FAILED', 'CANCELLED'
    )),
    root_count BIGINT NOT NULL DEFAULT 0 CHECK (root_count >= 0),
    discovered_directory_count BIGINT NOT NULL DEFAULT 0 CHECK (discovered_directory_count >= 0),
    completed_directory_count BIGINT NOT NULL DEFAULT 0 CHECK (completed_directory_count >= 0),
    observed_file_count BIGINT NOT NULL DEFAULT 0 CHECK (observed_file_count >= 0),
    unchanged_count BIGINT NOT NULL DEFAULT 0 CHECK (unchanged_count >= 0),
    add_count BIGINT NOT NULL DEFAULT 0 CHECK (add_count >= 0),
    change_count BIGINT NOT NULL DEFAULT 0 CHECK (change_count >= 0),
    remove_count BIGINT NOT NULL DEFAULT 0 CHECK (remove_count >= 0),
    reappeared_count BIGINT NOT NULL DEFAULT 0 CHECK (reappeared_count >= 0),
    applied_delta_count BIGINT NOT NULL DEFAULT 0 CHECK (applied_delta_count >= 0),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT),
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT),
    indexed_at BIGINT,
    completed_at BIGINT
);

CREATE TABLE scan_manifest_roots (
    manifest_id TEXT NOT NULL REFERENCES scan_manifests(id) ON DELETE CASCADE,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('PENDING', 'SCANNING', 'COMPLETE', 'UNAVAILABLE', 'INCOMPLETE')),
    directory_count BIGINT NOT NULL DEFAULT 0 CHECK (directory_count >= 0),
    completed_directory_count BIGINT NOT NULL DEFAULT 0 CHECK (completed_directory_count >= 0),
    observed_file_count BIGINT NOT NULL DEFAULT 0 CHECK (observed_file_count >= 0),
    error TEXT,
    started_at BIGINT,
    finished_at BIGINT,
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT),
    PRIMARY KEY (manifest_id, library_root_id)
);

CREATE TABLE scan_manifest_directories (
    manifest_id TEXT NOT NULL,
    library_root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('PENDING', 'SCANNING', 'COMPLETE', 'FAILED')),
    attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT),
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT),
    PRIMARY KEY (manifest_id, library_root_id, relative_path),
    FOREIGN KEY (manifest_id, library_root_id)
        REFERENCES scan_manifest_roots(manifest_id, library_root_id) ON DELETE CASCADE
);

CREATE TABLE scan_manifest_entries (
    manifest_id TEXT NOT NULL,
    library_root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    observation_sequence BIGINT NOT NULL CHECK (observation_sequence > 0),
    entry_kind TEXT NOT NULL CHECK (entry_kind IN ('FILE', 'DIRECTORY')),
    size BIGINT NOT NULL CHECK (size >= 0),
    modified_at BIGINT NOT NULL,
    inode BIGINT,
    fingerprint BYTEA,
    observed_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT),
    PRIMARY KEY (manifest_id, library_root_id, relative_path, observation_sequence),
    FOREIGN KEY (manifest_id, library_root_id)
        REFERENCES scan_manifest_roots(manifest_id, library_root_id) ON DELETE CASCADE
);

CREATE TABLE scan_manifest_deltas (
    id TEXT PRIMARY KEY NOT NULL,
    manifest_id TEXT NOT NULL,
    library_root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    observation_sequence BIGINT,
    delta_kind TEXT NOT NULL CHECK (delta_kind IN ('ADD', 'CHANGE', 'REMOVE', 'REAPPEARED')),
    base_filesystem_entry_id TEXT,
    base_fingerprint BYTEA,
    state TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (state IN ('PENDING', 'APPLIED', 'CONFLICT', 'UNSTABLE', 'FAILED')),
    attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT),
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT),
    UNIQUE (manifest_id, library_root_id, relative_path),
    FOREIGN KEY (manifest_id, library_root_id)
        REFERENCES scan_manifest_roots(manifest_id, library_root_id) ON DELETE CASCADE,
    FOREIGN KEY (manifest_id, library_root_id, relative_path, observation_sequence)
        REFERENCES scan_manifest_entries(manifest_id, library_root_id, relative_path, observation_sequence)
        ON DELETE CASCADE
);

CREATE INDEX idx_scan_manifests_library_state
    ON scan_manifests(library_id, state, updated_at, id);
CREATE INDEX idx_scan_manifest_roots_state
    ON scan_manifest_roots(manifest_id, state, library_root_id);
CREATE INDEX idx_scan_manifest_directories_frontier
    ON scan_manifest_directories(manifest_id, library_root_id, state, relative_path);
CREATE INDEX idx_scan_manifest_entries_path
    ON scan_manifest_entries(manifest_id, library_root_id, relative_path, observation_sequence);
CREATE INDEX idx_scan_manifest_deltas_pending
    ON scan_manifest_deltas(manifest_id, state, library_root_id, relative_path);
