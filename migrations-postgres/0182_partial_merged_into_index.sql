DROP INDEX IF EXISTS idx_media_items_merged_into;

CREATE INDEX idx_media_items_merged_into
    ON media_items(merged_into_item_id)
    WHERE merged_into_item_id IS NOT NULL;
