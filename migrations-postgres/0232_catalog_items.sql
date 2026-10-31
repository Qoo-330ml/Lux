-- Rebuildable read model for list/detail paths. The authoritative tables remain media_items,
-- media_sources, item_images and media_content_restriction_index; this table only caches what
-- those paths read, so it can be dropped and rebuilt at any time.
CREATE TABLE catalog_items (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL,
    item_type TEXT NOT NULL,
    parent_id TEXT,
    series_id TEXT,
    season_number INTEGER,
    episode_number INTEGER,
    title TEXT NOT NULL,
    sort_title TEXT NOT NULL,
    original_title TEXT,
    production_year INTEGER,
    rating DOUBLE PRECISION,
    runtime_ticks BIGINT,
    added_at BIGINT NOT NULL,
    effective_added_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    removed_at BIGINT,
    merged_into TEXT,
    has_available_source INTEGER NOT NULL DEFAULT 0,
    default_source_id TEXT,
    poster_tag TEXT,
    backdrop_tag TEXT,
    thumb_tag TEXT,
    logo_tag TEXT,
    season_count BIGINT NOT NULL DEFAULT 0,
    episode_count BIGINT NOT NULL DEFAULT 0,
    latest_child_added_at BIGINT,
    restriction_code TEXT,
    metadata_state TEXT,
    nfo_fingerprint BYTEA,
    applied_seq BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX idx_catalog_items_sort ON catalog_items(library_id, sort_title, item_id);
CREATE INDEX idx_catalog_items_recent ON catalog_items(library_id, effective_added_at DESC, item_id);
CREATE INDEX idx_catalog_items_series ON catalog_items(series_id);
CREATE INDEX idx_catalog_items_parent ON catalog_items(parent_id);
CREATE INDEX idx_catalog_items_restriction ON catalog_items(restriction_code, item_id);

CREATE TABLE catalog_item_stats (
    scope TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    item_type TEXT NOT NULL,
    total BIGINT NOT NULL,
    PRIMARY KEY (scope, scope_id, item_type)
);
