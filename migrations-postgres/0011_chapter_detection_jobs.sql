CREATE TABLE chapter_detection_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    plugin_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'CANCELLED', 'FAILED')),
    concurrency BIGINT NOT NULL CHECK (concurrency BETWEEN 1 AND 16),
    intro_window_seconds BIGINT NOT NULL CHECK (intro_window_seconds BETWEEN 15 AND 300),
    credits_window_seconds BIGINT NOT NULL CHECK (credits_window_seconds BETWEEN 15 AND 600),
    match_threshold DOUBLE PRECISION NOT NULL CHECK (match_threshold >= 0.0 AND match_threshold <= 1.0),
    cursor TEXT,
    processed_count BIGINT NOT NULL DEFAULT 0 CHECK (processed_count >= 0),
    total_count BIGINT NOT NULL DEFAULT 0 CHECK (total_count >= 0),
    cancel_requested BIGINT NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
);

CREATE INDEX idx_chapter_detection_jobs_status
    ON chapter_detection_jobs(status, created_at, id);
CREATE UNIQUE INDEX idx_chapter_detection_jobs_one_active_library
    ON chapter_detection_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');

CREATE TABLE chapter_detection_job_items (
    job_id TEXT NOT NULL REFERENCES chapter_detection_jobs(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    season_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    source_fingerprint BYTEA NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'SKIPPED', 'FAILED')),
    error TEXT,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, source_id)
);

CREATE INDEX idx_chapter_detection_items_pending
    ON chapter_detection_job_items(job_id, status, season_id, source_id);
