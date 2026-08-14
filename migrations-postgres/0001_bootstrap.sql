CREATE OR REPLACE FUNCTION unixepoch() RETURNS BIGINT
LANGUAGE SQL STABLE
AS $$ SELECT FLOOR(EXTRACT(EPOCH FROM clock_timestamp()))::BIGINT $$;

CREATE TABLE lux_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    username_normalized TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    is_disabled BIGINT NOT NULL DEFAULT 0 CHECK (is_disabled IN (0, 1)),
    is_admin BIGINT NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
    can_manage_server BIGINT NOT NULL DEFAULT 0 CHECK (can_manage_server IN (0, 1)),
    can_remote_access BIGINT NOT NULL DEFAULT 0 CHECK (can_remote_access IN (0, 1)),
    can_download BIGINT NOT NULL DEFAULT 0 CHECK (can_download IN (0, 1)),
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    last_login_at BIGINT
);
CREATE TABLE web_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    session_token_hash BYTEA NOT NULL UNIQUE,
    csrf_token_hash BYTEA NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    expires_at BIGINT NOT NULL,
    last_seen_at BIGINT,
    revoked_at BIGINT
);
CREATE INDEX web_sessions_user_id_idx ON web_sessions(user_id);
CREATE INDEX web_sessions_active_idx ON web_sessions(session_token_hash, expires_at, revoked_at);
CREATE TABLE access_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    token_hash BYTEA NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    client_name TEXT NOT NULL,
    device_name TEXT NOT NULL,
    client_version TEXT NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    last_seen_at BIGINT,
    revoked_at BIGINT
);
CREATE INDEX access_tokens_user_id_idx ON access_tokens(user_id);
CREATE INDEX access_tokens_active_idx ON access_tokens(token_hash, revoked_at);
CREATE TABLE libraries (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('MOVIE', 'SERIES', 'MIXED')),
    is_enabled BIGINT NOT NULL DEFAULT 1 CHECK (is_enabled IN (0, 1)),
    realtime_watch_enabled BIGINT NOT NULL DEFAULT 0 CHECK (realtime_watch_enabled IN (0, 1)),
    incremental_schedule TEXT,
    reconciliation_schedule TEXT,
    metadata_schedule TEXT,
    scan_concurrency BIGINT NOT NULL DEFAULT 2 CHECK (scan_concurrency > 0),
    probe_concurrency BIGINT NOT NULL DEFAULT 1 CHECK (probe_concurrency > 0),
    last_scan_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
, scraper_id TEXT, cover_image_path TEXT, cover_image_content_type TEXT, cover_image_size BIGINT, cover_image_tag TEXT, media_strategy_json TEXT);
CREATE TABLE library_roots (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    canonical_path TEXT NOT NULL,
    display_path TEXT NOT NULL,
    is_available BIGINT NOT NULL CHECK (is_available IN (0, 1)),
    is_writable BIGINT NOT NULL CHECK (is_writable IN (0, 1)),
    last_checked_at BIGINT NOT NULL DEFAULT (unixepoch()),
    unavailable_since BIGINT,
    scan_cursor TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE (library_id, canonical_path)
);
CREATE INDEX idx_library_roots_library_id ON library_roots(library_id);
CREATE INDEX idx_library_roots_canonical_path ON library_roots(canonical_path);
CREATE TABLE filesystem_entries (
    id TEXT PRIMARY KEY NOT NULL,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    entry_kind TEXT NOT NULL CHECK (entry_kind IN ('FILE', 'DIRECTORY')),
    size BIGINT NOT NULL,
    modified_at BIGINT NOT NULL,
    inode BIGINT,
    fingerprint BYTEA,
    last_seen_generation TEXT NOT NULL,
    is_missing BIGINT NOT NULL DEFAULT 0 CHECK (is_missing IN (0, 1)),
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE (library_root_id, relative_path)
);
CREATE TABLE media_items (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    item_type TEXT NOT NULL CHECK (item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE', 'BOX_SET', 'FOLDER', 'UNRESOLVED')),
    parent_id TEXT,
    series_id TEXT,
    season_number BIGINT,
    episode_number BIGINT,
    absolute_number BIGINT,
    title TEXT NOT NULL,
    sort_title TEXT NOT NULL,
    original_title TEXT,
    overview TEXT,
    production_year BIGINT,
    premiere_date TEXT,
    runtime_ticks BIGINT,
    provider_ids_json TEXT,
    metadata_provenance_json TEXT,
    locked_fields_json TEXT,
    identification_status TEXT NOT NULL CHECK (identification_status IN ('LOCAL_CONFIRMED', 'ONLINE_CONFIRMED', 'PENDING', 'FAILED')),
    added_at BIGINT NOT NULL DEFAULT (unixepoch()),
    removed_at BIGINT, metadata_fingerprint BYTEA, identity_key TEXT, rating DOUBLE PRECISION, rating_source TEXT, last_air_date TEXT, status TEXT, original_language TEXT, has_available_source BIGINT NOT NULL DEFAULT 0
CHECK (has_available_source IN (0, 1)),
);
CREATE TABLE media_sources (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('LOCAL_FILE', 'STRM_URL')),
    filesystem_entry_id TEXT REFERENCES filesystem_entries(id) ON DELETE CASCADE,
    edition_name TEXT,
    quality_label TEXT,
    container TEXT,
    size BIGINT,
    bitrate BIGINT,
    duration_ticks BIGINT,
    external_url TEXT,
    is_default BIGINT NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    probe_status TEXT NOT NULL DEFAULT 'PENDING',
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()), probe_error TEXT,
    UNIQUE (filesystem_entry_id)
);
CREATE INDEX idx_filesystem_entries_root_path ON filesystem_entries(library_root_id, relative_path);
CREATE INDEX idx_media_items_library_sort ON media_items(library_id, sort_title, id);
CREATE INDEX idx_media_sources_item_id ON media_sources(item_id);
CREATE TABLE item_images (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    image_type TEXT NOT NULL,
    image_index BIGINT NOT NULL DEFAULT 0,
    local_path TEXT NOT NULL,
    width BIGINT,
    height BIGINT,
    file_size BIGINT,
    content_tag TEXT,
    source TEXT NOT NULL DEFAULT 'LOCAL',
    language TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE (item_id, image_type, image_index)
);
CREATE INDEX idx_item_images_item_id ON item_images(item_id);
CREATE TABLE media_streams (
    id TEXT PRIMARY KEY NOT NULL,
    media_source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    stream_index BIGINT NOT NULL,
    stream_type TEXT NOT NULL CHECK (stream_type IN ('VIDEO', 'AUDIO', 'SUBTITLE')),
    codec TEXT,
    language TEXT,
    title TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()), external_path TEXT, is_external BIGINT NOT NULL DEFAULT 0 CHECK (is_external IN (0, 1)), is_default BIGINT NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)), is_forced BIGINT NOT NULL DEFAULT 0 CHECK (is_forced IN (0, 1)), details_json TEXT,
    UNIQUE (media_source_id, stream_index)
);
CREATE INDEX idx_media_streams_source_id ON media_streams(media_source_id);
CREATE TABLE user_library_access (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    can_view BIGINT NOT NULL DEFAULT 0 CHECK (can_view IN (0, 1)),
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (user_id, library_id)
);
CREATE INDEX idx_user_library_access_library_id ON user_library_access(library_id, can_view);
CREATE TABLE scan_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    job_type TEXT NOT NULL CHECK (job_type IN ('RECONCILE_LIBRARY', 'INCREMENTAL_SCAN')),
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'CANCELLED', 'FAILED')),
    generation TEXT NOT NULL,
    cursor TEXT,
    processed_count BIGINT NOT NULL DEFAULT 0,
    total_count BIGINT NOT NULL DEFAULT 0,
    cancel_requested BIGINT NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
, discovery_completed BIGINT NOT NULL DEFAULT 1
        CHECK (discovery_completed IN (0, 1)));
CREATE INDEX idx_scan_jobs_library_status ON scan_jobs(library_id, status, created_at);
CREATE UNIQUE INDEX idx_scan_jobs_one_active
    ON scan_jobs(library_id, job_type)
    WHERE status IN ('PENDING', 'RUNNING');
CREATE TABLE scheduled_task_configs (
    owner_type TEXT NOT NULL CHECK (owner_type IN ('GLOBAL', 'LIBRARY')),
    owner_id TEXT NOT NULL,
    task_type TEXT NOT NULL,
    cron_or_interval TEXT,
    is_enabled BIGINT NOT NULL DEFAULT 1 CHECK (is_enabled IN (0, 1)),
    resource_limit_json TEXT NOT NULL DEFAULT '{}',
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()), task_name TEXT NOT NULL DEFAULT '', task_description TEXT NOT NULL DEFAULT '', source_type TEXT NOT NULL DEFAULT 'SYSTEM', plugin_id TEXT,
    PRIMARY KEY (owner_type, owner_id, task_type)
);
CREATE INDEX idx_scheduled_task_configs_owner ON scheduled_task_configs(owner_type, owner_id);
CREATE TABLE metadata_candidates (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    candidate_json TEXT NOT NULL,
    score DOUBLE PRECISION NOT NULL CHECK (score >= 0 AND score <= 100),
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'SELECTED', 'REJECTED', 'EXPIRED')),
    expires_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_metadata_candidates_status ON metadata_candidates(status, created_at, id);
CREATE INDEX idx_metadata_candidates_item ON metadata_candidates(item_id, status, created_at, id);
CREATE UNIQUE INDEX idx_media_items_identity_key
ON media_items(identity_key)
WHERE identity_key IS NOT NULL;
CREATE TABLE user_item_state (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    position_ticks BIGINT NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    is_played BIGINT NOT NULL DEFAULT 0 CHECK (is_played IN (0, 1)),
    is_favorite BIGINT NOT NULL DEFAULT 0 CHECK (is_favorite IN (0, 1)),
    play_count BIGINT NOT NULL DEFAULT 0 CHECK (play_count >= 0),
    last_played_at BIGINT,
    version BIGINT NOT NULL DEFAULT 0 CHECK (version >= 0),
    PRIMARY KEY (user_id, item_id)
);
CREATE INDEX idx_user_item_state_next_up
ON user_item_state(user_id, is_played, position_ticks, last_played_at);
CREATE INDEX idx_media_streams_external_path
ON media_streams(media_source_id, external_path)
WHERE external_path IS NOT NULL;
CREATE TABLE playback_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    media_source_id TEXT REFERENCES media_sources(id) ON DELETE SET NULL,
    play_session_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    client TEXT,
    device_name TEXT,
    state TEXT NOT NULL CHECK (state IN ('PLAYING', 'PAUSED', 'STOPPED')),
    position_ticks BIGINT NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    duration_ticks BIGINT CHECK (duration_ticks IS NULL OR duration_ticks >= 0),
    is_paused BIGINT NOT NULL DEFAULT 0 CHECK (is_paused IN (0, 1)),
    started_at BIGINT NOT NULL DEFAULT (unixepoch()),
    last_event_at BIGINT NOT NULL DEFAULT (unixepoch()), remote_ip TEXT, client_version TEXT, device_type TEXT,
    UNIQUE (user_id, play_session_id)
);
CREATE INDEX idx_playback_sessions_active
ON playback_sessions(user_id, state, last_event_at);
CREATE TABLE server_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE item_aliases (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    alias TEXT NOT NULL,
    language TEXT,
    alias_normalized TEXT NOT NULL,
    UNIQUE (item_id, alias_normalized)
);
CREATE INDEX idx_item_aliases_item_id ON item_aliases(item_id);

CREATE TABLE media_search (
    item_id TEXT PRIMARY KEY REFERENCES media_items(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    sort_title TEXT NOT NULL,
    original_title TEXT NOT NULL DEFAULT '',
    aliases TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_media_search_title ON media_search(title);
CREATE INDEX idx_media_search_sort_title ON media_search(sort_title);

CREATE TABLE collections (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL UNIQUE REFERENCES media_items(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    title TEXT NOT NULL,
    overview TEXT,
    poster_path TEXT,
    backdrop_path TEXT,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE (library_id, provider, provider_id)
);
CREATE TABLE collection_items (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    sort_order BIGINT NOT NULL,
    PRIMARY KEY (collection_id, item_id)
);
CREATE INDEX idx_collection_items_item ON collection_items(item_id, collection_id);
CREATE TABLE audit_events (
    id TEXT PRIMARY KEY NOT NULL,
    actor_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    event_type TEXT NOT NULL,
    target_type TEXT,
    target_id TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at BIGINT NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_audit_events_created_at ON audit_events(created_at DESC, id DESC);
CREATE INDEX idx_audit_events_actor ON audit_events(actor_user_id, created_at DESC);
CREATE TABLE scan_job_events (
    id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    level TEXT NOT NULL CHECK (level IN ('INFO', 'WARN', 'ERROR')),
    event_code TEXT NOT NULL,
    message TEXT NOT NULL,
    details_json TEXT NOT NULL DEFAULT '{}',
    created_at BIGINT NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX idx_scan_job_events_job_created
    ON scan_job_events(job_id, created_at DESC, id DESC);
CREATE INDEX idx_scan_job_events_code
    ON scan_job_events(event_code, created_at DESC, id DESC);
CREATE TABLE metadata_reidentify_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('QUEUED', 'RUNNING', 'COMPLETED', 'FAILED', 'CANCELLED')),
    processed_count BIGINT NOT NULL DEFAULT 0,
    total_count BIGINT NOT NULL DEFAULT 0,
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
, mode TEXT NOT NULL DEFAULT 'REIDENTIFY'
    CHECK (mode IN ('REIDENTIFY', 'FILL_MISSING', 'FULL_REFRESH')), cancel_requested BIGINT NOT NULL DEFAULT 0
        CHECK (cancel_requested IN (0, 1)));
CREATE TABLE metadata_reidentify_job_items (
    job_id TEXT NOT NULL REFERENCES metadata_reidentify_jobs(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'FAILED')),
    candidate_count BIGINT NOT NULL DEFAULT 0,
    error TEXT,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, item_id)
);
CREATE INDEX idx_metadata_reidentify_items_status
    ON metadata_reidentify_job_items(job_id, status, item_id);
CREATE TABLE installed_plugins (
    plugin_id TEXT PRIMARY KEY NOT NULL,
    is_enabled BIGINT NOT NULL DEFAULT 1 CHECK (is_enabled IN (0, 1)),
    installed_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);
CREATE TABLE strm_probe_jobs (
    id TEXT PRIMARY KEY NOT NULL,
    operation_id TEXT NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'COMPLETED', 'CANCELLED', 'FAILED')),
    concurrency BIGINT NOT NULL CHECK (concurrency BETWEEN 1 AND 64),
    include_ready BIGINT NOT NULL DEFAULT 0 CHECK (include_ready IN (0, 1)),
    write_sidecars BIGINT NOT NULL DEFAULT 0 CHECK (write_sidecars IN (0, 1)),
    cursor TEXT,
    processed_count BIGINT NOT NULL DEFAULT 0,
    total_count BIGINT NOT NULL DEFAULT 0,
    cancel_requested BIGINT NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
);
CREATE INDEX idx_strm_probe_jobs_operation
    ON strm_probe_jobs(operation_id, created_at, id);
CREATE INDEX idx_strm_probe_jobs_status
    ON strm_probe_jobs(status, created_at, id);
CREATE UNIQUE INDEX idx_strm_probe_jobs_one_active_library
    ON strm_probe_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');
CREATE TABLE danmaku_tracks (
    id TEXT PRIMARY KEY NOT NULL,
    media_source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format = 'XML'),
    provider TEXT,
    provider_anime_id TEXT,
    provider_episode_id TEXT,
    fingerprint BYTEA,
    status TEXT NOT NULL CHECK (status IN ('READY', 'MISSING', 'INVALID', 'FAILED')),
    error_code TEXT,
    last_checked_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
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
    overwrite BIGINT NOT NULL DEFAULT 0 CHECK (overwrite IN (0, 1)),
    concurrency BIGINT NOT NULL CHECK (concurrency BETWEEN 1 AND 64),
    total_count BIGINT NOT NULL DEFAULT 0,
    processed_count BIGINT NOT NULL DEFAULT 0,
    success_count BIGINT NOT NULL DEFAULT 0,
    skipped_count BIGINT NOT NULL DEFAULT 0,
    failed_count BIGINT NOT NULL DEFAULT 0,
    cancel_requested BIGINT NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    error TEXT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    started_at BIGINT,
    finished_at BIGINT
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
    attempts BIGINT NOT NULL DEFAULT 0,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE (job_id, media_source_id)
);
CREATE INDEX idx_danmaku_match_job_items_status
    ON danmaku_match_job_items(job_id, status, id);
CREATE UNIQUE INDEX idx_danmaku_match_jobs_one_active_library
    ON danmaku_match_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');
CREATE TABLE scan_job_paths (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    change_kind TEXT NOT NULL CHECK (change_kind IN ('CREATE', 'MODIFY', 'RENAME', 'REMOVE')),
    processed_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, library_root_id, relative_path)
);
CREATE INDEX idx_scan_job_paths_pending
    ON scan_job_paths(job_id, processed_at, created_at, relative_path);
CREATE TABLE reconciliation_scan_entries (
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    entry_type TEXT NOT NULL CHECK (entry_type IN ('DIRECTORY', 'FILE')),
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (job_id, library_root_id, entry_type, relative_path)
);
CREATE INDEX idx_reconciliation_scan_entries_pending
    ON reconciliation_scan_entries(job_id, entry_type, library_root_id, relative_path);
CREATE INDEX idx_media_items_parent_removed
    ON media_items(parent_id, removed_at);
CREATE INDEX idx_media_items_series_removed
    ON media_items(series_id, removed_at);

INSERT INTO lux_meta (key, value) VALUES ('schema', 'bootstrap')
ON CONFLICT (key) DO NOTHING;

CREATE OR REPLACE FUNCTION lux_refresh_media_search_item()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        DELETE FROM media_search WHERE item_id = OLD.id;
        RETURN OLD;
    END IF;

    INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
    SELECT NEW.id,
           NEW.title,
           NEW.sort_title,
           COALESCE(NEW.original_title, ''),
           COALESCE((SELECT string_agg(alias, ' ') FROM item_aliases WHERE item_id = NEW.id), '')
    ON CONFLICT (item_id) DO UPDATE SET
        title = EXCLUDED.title,
        sort_title = EXCLUDED.sort_title,
        original_title = EXCLUDED.original_title,
        aliases = EXCLUDED.aliases;
    RETURN NEW;
END;
$$;

CREATE TRIGGER media_items_search_ai
AFTER INSERT ON media_items
FOR EACH ROW EXECUTE FUNCTION lux_refresh_media_search_item();
CREATE TRIGGER media_items_search_au
AFTER UPDATE OF title, sort_title, original_title ON media_items
FOR EACH ROW EXECUTE FUNCTION lux_refresh_media_search_item();
CREATE TRIGGER media_items_search_ad
AFTER DELETE ON media_items
FOR EACH ROW EXECUTE FUNCTION lux_refresh_media_search_item();

CREATE OR REPLACE FUNCTION lux_refresh_media_search_aliases()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    target_item_id TEXT;
BEGIN
    target_item_id := CASE WHEN TG_OP = 'DELETE' THEN OLD.item_id ELSE NEW.item_id END;
    UPDATE media_search
    SET aliases = COALESCE(
        (SELECT string_agg(alias, ' ') FROM item_aliases WHERE item_id = target_item_id),
        ''
    )
    WHERE item_id = target_item_id;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END;
$$;

CREATE TRIGGER item_aliases_search_ai
AFTER INSERT ON item_aliases
FOR EACH ROW EXECUTE FUNCTION lux_refresh_media_search_aliases();
CREATE TRIGGER item_aliases_search_au
AFTER UPDATE OF alias, alias_normalized, item_id ON item_aliases
FOR EACH ROW EXECUTE FUNCTION lux_refresh_media_search_aliases();
CREATE TRIGGER item_aliases_search_ad
AFTER DELETE ON item_aliases
FOR EACH ROW EXECUTE FUNCTION lux_refresh_media_search_aliases();

CREATE OR REPLACE FUNCTION lux_refresh_item_availability()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    source_item_id TEXT;
BEGIN
    IF TG_TABLE_NAME = 'media_sources' THEN
        source_item_id := CASE WHEN TG_OP = 'DELETE' THEN OLD.item_id ELSE NEW.item_id END;
        UPDATE media_items
        SET has_available_source = CASE WHEN EXISTS (
            SELECT 1
            FROM media_sources ms
            JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
            WHERE ms.item_id = media_items.id AND fe.is_missing = 0
        ) THEN 1 ELSE 0 END
        WHERE id = source_item_id OR (TG_OP = 'UPDATE' AND id = OLD.item_id);
        RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
    END IF;

    UPDATE media_items
    SET has_available_source = CASE WHEN EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id AND fe.is_missing = 0
    ) THEN 1 ELSE 0 END
    WHERE id IN (
        SELECT item_id FROM media_sources WHERE filesystem_entry_id = NEW.id
    );
    RETURN NEW;
END;
$$;

CREATE TRIGGER media_sources_availability_ai
AFTER INSERT ON media_sources
FOR EACH ROW EXECUTE FUNCTION lux_refresh_item_availability();
CREATE TRIGGER media_sources_availability_au
AFTER UPDATE OF item_id, filesystem_entry_id ON media_sources
FOR EACH ROW EXECUTE FUNCTION lux_refresh_item_availability();
CREATE TRIGGER media_sources_availability_ad
AFTER DELETE ON media_sources
FOR EACH ROW EXECUTE FUNCTION lux_refresh_item_availability();
CREATE TRIGGER filesystem_entries_availability_au
AFTER UPDATE OF is_missing ON filesystem_entries
FOR EACH ROW EXECUTE FUNCTION lux_refresh_item_availability();
