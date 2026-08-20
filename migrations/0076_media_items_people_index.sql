CREATE INDEX IF NOT EXISTS idx_media_items_people_visible
    ON media_items(library_id, id);
