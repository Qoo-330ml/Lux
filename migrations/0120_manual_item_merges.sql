ALTER TABLE media_items
    ADD COLUMN merged_into_item_id TEXT REFERENCES media_items(id) ON DELETE SET NULL;

CREATE INDEX idx_media_items_merged_into
    ON media_items(merged_into_item_id);
