CREATE INDEX IF NOT EXISTS idx_media_items_library_type_visible
    ON media_items(library_id, item_type, id)
    WHERE removed_at IS NULL;
