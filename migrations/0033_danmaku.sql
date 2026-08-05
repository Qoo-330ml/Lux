CREATE TABLE danmaku_tracks (
    id TEXT PRIMARY KEY NOT NULL,
    media_source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format = 'XML'),
    provider TEXT,
    provider_anime_id TEXT,
    provider_episode_id TEXT,
    fingerprint BLOB,
    status TEXT NOT NULL CHECK (status IN ('READY', 'MISSING', 'INVALID', 'FAILED')),
    error_code TEXT,
    last_checked_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (media_source_id)
);

CREATE INDEX idx_danmaku_tracks_status
    ON danmaku_tracks(status, updated_at, id);

CREATE INDEX idx_danmaku_tracks_provider_episode
    ON danmaku_tracks(provider, provider_episode_id);

CREATE TABLE danmaku_match_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'FAILED', 'CANCELLED')),
    overwrite INTEGER NOT NULL DEFAULT 0 CHECK (overwrite IN (0, 1)),
    concurrency INTEGER NOT NULL CHECK (concurrency BETWEEN 1 AND 64),
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

CREATE INDEX idx_danmaku_match_jobs_status
    ON danmaku_match_jobs(status, created_at, id);

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

CREATE INDEX idx_danmaku_match_job_items_status
    ON danmaku_match_job_items(job_id, status, id);
