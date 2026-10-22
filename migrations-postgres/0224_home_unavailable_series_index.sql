-- SQLite has had this partial index since 0112, but PostgreSQL never received it, so the bounded
-- home query's unavailable-series branch had to scan the whole media_items relation.
CREATE INDEX IF NOT EXISTS idx_media_items_home_unavailable_series
    ON media_items(library_id, added_at DESC, sort_title, id)
    WHERE removed_at IS NULL
      AND item_type = 'SERIES'
      AND has_available_source = 0;
