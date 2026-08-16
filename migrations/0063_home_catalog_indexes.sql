CREATE INDEX IF NOT EXISTS idx_media_items_library_added_visible
    ON media_items(library_id, added_at DESC, sort_title, id)
    WHERE removed_at IS NULL
      AND item_type <> 'FOLDER'
      AND has_available_source = 1;

CREATE INDEX IF NOT EXISTS idx_media_items_parent_available
    ON media_items(parent_id, removed_at, has_available_source);

CREATE INDEX IF NOT EXISTS idx_media_items_series_available
    ON media_items(series_id, removed_at, has_available_source);
