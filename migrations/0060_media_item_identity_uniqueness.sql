-- no-transaction
PRAGMA foreign_keys = OFF;

DROP TRIGGER trg_media_sources_availability_insert;
DROP TRIGGER trg_media_sources_availability_update;
DROP TRIGGER trg_media_sources_availability_delete;
DROP TRIGGER trg_filesystem_entries_availability_update;

DROP INDEX idx_media_items_library_sort;
DROP INDEX idx_media_items_parent_removed;
DROP INDEX idx_media_items_series_removed;
DROP INDEX idx_media_items_identity_key;

CREATE TABLE media_items_new (
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
    metadata_fingerprint BLOB,
    identity_key TEXT,
    rating REAL,
    rating_source TEXT,
    last_air_date TEXT,
    status TEXT,
    original_language TEXT,
    has_available_source INTEGER NOT NULL DEFAULT 0 CHECK (has_available_source IN (0, 1)),
    thumbnail_fallback_required INTEGER NOT NULL DEFAULT 0 CHECK (thumbnail_fallback_required IN (0, 1)),
    nfo_metadata_json TEXT,
    nfo_metadata_fingerprint BLOB
);

INSERT INTO media_items_new (
    id, library_id, item_type, parent_id, series_id,
    season_number, episode_number, absolute_number,
    title, sort_title, original_title, overview, production_year,
    premiere_date, runtime_ticks, provider_ids_json, metadata_provenance_json,
    locked_fields_json, identification_status, added_at, removed_at,
    metadata_fingerprint, identity_key, rating, rating_source, last_air_date,
    status, original_language, has_available_source, thumbnail_fallback_required,
    nfo_metadata_json, nfo_metadata_fingerprint
)
SELECT
    id, library_id, item_type, parent_id, series_id,
    season_number, episode_number, absolute_number,
    title, sort_title, original_title, overview, production_year,
    premiere_date, runtime_ticks, provider_ids_json, metadata_provenance_json,
    locked_fields_json, identification_status, added_at, removed_at,
    metadata_fingerprint, identity_key, rating, rating_source, last_air_date,
    status, original_language, has_available_source, thumbnail_fallback_required,
    nfo_metadata_json, nfo_metadata_fingerprint
FROM media_items;

DROP TABLE media_items;
ALTER TABLE media_items_new RENAME TO media_items;

CREATE INDEX idx_media_items_library_sort ON media_items(library_id, sort_title, id);
CREATE UNIQUE INDEX idx_media_items_identity_key
    ON media_items(identity_key)
    WHERE identity_key IS NOT NULL;
CREATE INDEX idx_media_items_parent_removed
    ON media_items(parent_id, removed_at);
CREATE INDEX idx_media_items_series_removed
    ON media_items(series_id, removed_at);

CREATE TRIGGER trg_media_sources_availability_insert
AFTER INSERT ON media_sources
BEGIN
    UPDATE media_items
    SET has_available_source = 1
    WHERE id = NEW.item_id
      AND EXISTS (
          SELECT 1
          FROM filesystem_entries
          WHERE id = NEW.filesystem_entry_id
            AND is_missing = 0
      );
END;

CREATE TRIGGER trg_media_sources_availability_update
AFTER UPDATE OF item_id, filesystem_entry_id ON media_sources
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id IN (OLD.item_id, NEW.item_id);
END;

CREATE TRIGGER trg_media_sources_availability_delete
AFTER DELETE ON media_sources
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id = OLD.item_id;
END;

CREATE TRIGGER trg_filesystem_entries_availability_update
AFTER UPDATE OF is_missing ON filesystem_entries
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id IN (
        SELECT item_id
        FROM media_sources
        WHERE filesystem_entry_id = NEW.id
    );
END;

PRAGMA foreign_keys = ON;
