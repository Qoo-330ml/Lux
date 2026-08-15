CREATE TABLE filesystem_entries (
    id TEXT PRIMARY KEY NOT NULL,
    library_root_id TEXT NOT NULL REFERENCES library_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    entry_kind TEXT NOT NULL CHECK (entry_kind IN ('FILE', 'DIRECTORY')),
    size INTEGER NOT NULL,
    modified_at INTEGER NOT NULL,
    inode INTEGER,
    fingerprint BLOB,
    last_seen_generation TEXT NOT NULL,
    is_missing INTEGER NOT NULL DEFAULT 0 CHECK (is_missing IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (library_root_id, relative_path)
);

CREATE TABLE media_items (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    item_type TEXT NOT NULL CHECK (item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE', 'BOX_SET', 'FOLDER', 'UNRESOLVED')),
    parent_id TEXT,
    series_id TEXT,
    season_number INTEGER,
    episode_number INTEGER,
    absolute_number INTEGER,
    title TEXT NOT NULL,
    sort_title TEXT NOT NULL,
    original_title TEXT,
    overview TEXT,
    production_year INTEGER,
    premiere_date TEXT,
    runtime_ticks INTEGER,
    provider_ids_json TEXT,
    metadata_provenance_json TEXT,
    locked_fields_json TEXT,
    identification_status TEXT NOT NULL CHECK (identification_status IN ('LOCAL_CONFIRMED', 'ONLINE_CONFIRMED', 'PENDING', 'FAILED')),
    added_at INTEGER NOT NULL DEFAULT (unixepoch()),
    removed_at INTEGER,
    UNIQUE (library_id, sort_title, production_year)
);

CREATE TABLE media_sources (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('LOCAL_FILE', 'STRM_URL')),
    filesystem_entry_id TEXT REFERENCES filesystem_entries(id) ON DELETE CASCADE,
    edition_name TEXT,
    quality_label TEXT,
    container TEXT,
    size INTEGER,
    bitrate INTEGER,
    duration_ticks INTEGER,
    external_url TEXT,
    is_default INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    probe_status TEXT NOT NULL DEFAULT 'PENDING',
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (filesystem_entry_id)
);

CREATE INDEX idx_filesystem_entries_root_path ON filesystem_entries(library_root_id, relative_path);
CREATE INDEX idx_media_items_library_sort ON media_items(library_id, sort_title, id);
CREATE INDEX idx_media_sources_item_id ON media_sources(item_id);
