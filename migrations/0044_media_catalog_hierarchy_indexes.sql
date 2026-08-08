CREATE INDEX idx_media_items_parent_removed
    ON media_items(parent_id, removed_at);

CREATE INDEX idx_media_items_series_removed
    ON media_items(series_id, removed_at);
