-- no-transaction
-- SQLite cannot alter a CHECK constraint in place, so rebuild the danmaku job tables.
PRAGMA foreign_keys = OFF;

DROP INDEX idx_danmaku_match_jobs_status;
DROP INDEX idx_danmaku_match_jobs_one_active_library;
DROP INDEX idx_danmaku_match_job_items_status;

ALTER TABLE danmaku_match_job_items RENAME TO danmaku_match_job_items_legacy;
ALTER TABLE danmaku_match_jobs RENAME TO danmaku_match_jobs_legacy;

CREATE TABLE danmaku_match_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'FAILED', 'CANCELLED')),
    overwrite INTEGER NOT NULL DEFAULT 0 CHECK (overwrite IN (0, 1)),
    concurrency INTEGER NOT NULL CHECK (concurrency BETWEEN 0 AND 64),
    total_count INTEGER NOT NULL DEFAULT 0,
    processed_count INTEGER NOT NULL DEFAULT 0,
    success_count INTEGER NOT NULL DEFAULT 0,
    skipped_count INTEGER NOT NULL DEFAULT 0,
    failed_count INTEGER NOT NULL DEFAULT 0,
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    error TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    started_at INTEGER,
    finished_at INTEGER
);

INSERT INTO danmaku_match_jobs (
    id, library_id, status, overwrite, concurrency, total_count,
    processed_count, success_count, skipped_count, failed_count,
    cancel_requested, error, created_at, updated_at, started_at, finished_at
)
SELECT
    id, library_id, status, overwrite, concurrency, total_count,
    processed_count, success_count, skipped_count, failed_count,
    cancel_requested, error, created_at, updated_at, started_at, finished_at
FROM danmaku_match_jobs_legacy;

CREATE TABLE danmaku_match_job_items (
    id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL REFERENCES danmaku_match_jobs(id) ON DELETE CASCADE,
    media_source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (
        status IN ('PENDING', 'RUNNING', 'MATCHED', 'WRITTEN', 'SKIPPED', 'FAILED', 'CANCELLED')
    ),
    provider_anime_id TEXT,
    provider_episode_id TEXT,
    error_code TEXT,
    error_message TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (job_id, media_source_id)
);

INSERT INTO danmaku_match_job_items (
    id, job_id, media_source_id, status, provider_anime_id,
    provider_episode_id, error_code, error_message, attempts, updated_at
)
SELECT
    id, job_id, media_source_id, status, provider_anime_id,
    provider_episode_id, error_code, error_message, attempts, updated_at
FROM danmaku_match_job_items_legacy;

DROP TABLE danmaku_match_job_items_legacy;
DROP TABLE danmaku_match_jobs_legacy;

CREATE INDEX idx_danmaku_match_jobs_status
    ON danmaku_match_jobs(status, created_at, id);
CREATE INDEX idx_danmaku_match_job_items_status
    ON danmaku_match_job_items(job_id, status, id);
CREATE UNIQUE INDEX idx_danmaku_match_jobs_one_active_library
    ON danmaku_match_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');

PRAGMA foreign_keys = ON;
