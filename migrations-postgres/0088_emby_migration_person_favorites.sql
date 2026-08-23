CREATE TABLE emby_migration_person_favorites (
    job_id TEXT NOT NULL REFERENCES emby_migration_jobs(id) ON DELETE CASCADE,
    emby_user_id TEXT NOT NULL,
    emby_person_id TEXT NOT NULL,
    emby_person_name TEXT NOT NULL,
    lux_user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    lux_person_id TEXT REFERENCES people(id) ON DELETE SET NULL,
    provider_ids_json TEXT NOT NULL DEFAULT '{}',
    match_method TEXT NOT NULL CHECK (match_method IN ('TMDB_ID', 'PROVIDER_ID', 'NAME', 'UNMATCHED', 'CONFLICT')),
    confidence BIGINT CHECK (confidence IS NULL OR (confidence >= 0 AND confidence <= 100)),
    status TEXT NOT NULL CHECK (status IN ('MATCHED', 'IMPORTED', 'UNMATCHED', 'CONFLICT', 'SKIPPED', 'FAILED')),
    state_hash TEXT NOT NULL,
    detail_json TEXT NOT NULL DEFAULT '{}',
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, emby_user_id, emby_person_id)
);

CREATE INDEX idx_emby_migration_person_favorites_status
    ON emby_migration_person_favorites(job_id, status, emby_user_id, emby_person_id);
CREATE INDEX idx_emby_migration_person_favorites_lux
    ON emby_migration_person_favorites(lux_user_id, lux_person_id, status);
