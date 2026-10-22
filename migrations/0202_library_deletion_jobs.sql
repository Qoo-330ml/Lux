ALTER TABLE libraries
    ADD COLUMN deletion_state TEXT NOT NULL DEFAULT 'ACTIVE'
        CHECK (deletion_state IN ('ACTIVE', 'DELETING'));

CREATE TABLE library_deletion_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL,
    library_name TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'FAILED', 'CANCELLED')),
    stage TEXT NOT NULL CHECK (
        stage IN (
            'CANCELLING_SCANS', 'MEDIA_ITEMS', 'FILESYSTEM_ENTRIES',
            'STRM_PROBE_JOBS', 'SCAN_JOB_DATA', 'SCAN_JOBS', 'FINALIZING', 'COMPLETED'
        )
    ),
    processed_count BIGINT NOT NULL DEFAULT 0 CHECK (processed_count >= 0),
    error_code TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
);

CREATE INDEX idx_library_deletion_jobs_created
    ON library_deletion_jobs(created_at DESC, id DESC);

CREATE INDEX idx_library_deletion_jobs_status_created
    ON library_deletion_jobs(status, created_at DESC, id DESC);

CREATE UNIQUE INDEX ux_library_deletion_jobs_one_active_library
    ON library_deletion_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');

CREATE INDEX idx_user_collection_source_preferences_media_source_id
    ON user_collection_source_preferences(media_source_id);

CREATE INDEX idx_user_collection_source_preferences_work_source
    ON user_collection_source_preferences(work_item_id, source_item_id);

CREATE INDEX idx_scan_job_file_changes_library_root_id
    ON scan_job_file_changes(library_root_id);
