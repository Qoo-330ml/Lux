-- Keep the unavailable-series branch ordered by the same keys used by the
-- bounded home query, so SQLite can stop after finding the requested rows.
CREATE INDEX idx_media_items_home_unavailable_series
    ON media_items(library_id, added_at DESC, sort_title, id)
    WHERE removed_at IS NULL
      AND item_type = 'SERIES'
      AND has_available_source = 0;
