-- Materialised ordering key for the home "latest by library" rail: the series timestamp is the
-- newest available episode, every other item keeps its own added_at. Without the column the
-- ordering expression cannot use an index.
ALTER TABLE catalog_items ADD COLUMN latest_added_at INTEGER;
UPDATE catalog_items SET latest_added_at = effective_added_at WHERE latest_added_at IS NULL;

CREATE INDEX idx_catalog_items_latest_added
    ON catalog_items(library_id, latest_added_at DESC, item_id);
