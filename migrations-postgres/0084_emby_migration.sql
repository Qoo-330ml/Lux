CREATE TABLE emby_migration_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    plugin_id TEXT NOT NULL,
    created_by_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    source_label TEXT NOT NULL,
    source_base_url TEXT NOT NULL,
    secret_ref TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'CANCELLED', 'FAILED')),
    phase TEXT NOT NULL CHECK (phase IN ('TESTING', 'USERS', 'ITEMS', 'MATCHING', 'IMPORTING', 'FINALIZING')),
    dry_run BIGINT NOT NULL DEFAULT 1 CHECK (dry_run IN (0, 1)),
    merge_policy TEXT NOT NULL CHECK (merge_policy IN ('MERGE', 'OVERWRITE', 'SKIP')),
    cursor_json TEXT NOT NULL DEFAULT '{}',
    processed_count BIGINT NOT NULL DEFAULT 0 CHECK (processed_count >= 0),
    total_count BIGINT NOT NULL DEFAULT 0 CHECK (total_count >= 0),
    matched_count BIGINT NOT NULL DEFAULT 0 CHECK (matched_count >= 0),
    skipped_count BIGINT NOT NULL DEFAULT 0 CHECK (skipped_count >= 0),
    failed_count BIGINT NOT NULL DEFAULT 0 CHECK (failed_count >= 0),
    run_token TEXT,
    cancel_requested BIGINT NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
);
CREATE INDEX idx_emby_migration_jobs_status ON emby_migration_jobs(status, created_at, id);

CREATE TABLE emby_migration_user_links (
    job_id TEXT NOT NULL REFERENCES emby_migration_jobs(id) ON DELETE CASCADE,
    emby_user_id TEXT NOT NULL,
    emby_username TEXT NOT NULL,
    lux_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'AUTO_CREATED', 'LINKED', 'SKIPPED', 'CONFLICT', 'FAILED')),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, emby_user_id)
);
CREATE INDEX idx_emby_migration_user_links_lux_user ON emby_migration_user_links(lux_user_id, status);

CREATE TABLE emby_migration_item_matches (
    job_id TEXT NOT NULL REFERENCES emby_migration_jobs(id) ON DELETE CASCADE,
    emby_item_id TEXT NOT NULL,
    emby_item_type TEXT NOT NULL,
    lux_item_id TEXT REFERENCES media_items(id) ON DELETE SET NULL,
    match_method TEXT NOT NULL CHECK (match_method IN ('TMDB_ID', 'PROVIDER_ID', 'EPISODE_KEY', 'TITLE_YEAR', 'UNMATCHED', 'CONFLICT')),
    confidence BIGINT CHECK (confidence IS NULL OR (confidence >= 0 AND confidence <= 100)),
    status TEXT NOT NULL CHECK (status IN ('MATCHED', 'UNMATCHED', 'CONFLICT', 'SKIPPED')),
    detail_json TEXT NOT NULL DEFAULT '{}',
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, emby_item_id)
);
CREATE INDEX idx_emby_migration_item_matches_status ON emby_migration_item_matches(job_id, status, emby_item_id);

CREATE TABLE emby_migration_import_records (
    job_id TEXT NOT NULL REFERENCES emby_migration_jobs(id) ON DELETE CASCADE,
    emby_user_id TEXT NOT NULL,
    emby_item_id TEXT NOT NULL,
    lux_user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    lux_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    state_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('IMPORTED', 'SKIPPED', 'FAILED')),
    error TEXT,
    imported_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, emby_user_id, emby_item_id)
);
CREATE INDEX idx_emby_migration_import_records_lux ON emby_migration_import_records(lux_user_id, lux_item_id);

CREATE TABLE emby_migration_user_bindings (
    lux_user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    source_base_url TEXT NOT NULL,
    emby_user_id TEXT NOT NULL,
    emby_username TEXT NOT NULL,
    password_pending BIGINT NOT NULL DEFAULT 1 CHECK (password_pending IN (0, 1)),
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE (source_base_url, emby_user_id)
);
CREATE INDEX idx_emby_migration_user_bindings_username ON emby_migration_user_bindings(source_base_url, emby_username);

CREATE TABLE playback_history_events (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (event_type IN ('PLAY_STARTED', 'PLAY_PROGRESS', 'PAUSED', 'STOPPED', 'MARKED_PLAYED')),
    position_ticks BIGINT NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    duration_ticks BIGINT CHECK (duration_ticks IS NULL OR duration_ticks >= 0),
    occurred_at BIGINT NOT NULL,
    source TEXT NOT NULL,
    source_event_key TEXT NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE (source, source_event_key)
);
CREATE INDEX idx_playback_history_events_user_time ON playback_history_events(user_id, occurred_at DESC, id DESC);
CREATE INDEX idx_playback_history_events_item_time ON playback_history_events(item_id, occurred_at DESC, id DESC);
