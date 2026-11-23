-- "Recently added" orders a series by the newest available episode, so a show that receives a new
-- episode moves back to the top of its library. `recent_added_at` drives the ordering key;
-- `latest_episode_added_at_sort` is the same episode timestamp on its own so the ordering index can
-- store it (SQLite rejects subqueries in index expressions). `effective_added_at` stays the series'
-- own `added_at` because it also drives the date filters and the "earliest added_at of a library"
-- queries.
ALTER TABLE catalog_items ADD COLUMN recent_added_at INTEGER;
ALTER TABLE catalog_items ADD COLUMN latest_episode_added_at_sort INTEGER;

UPDATE catalog_items
SET recent_added_at = COALESCE(latest_added_at, effective_added_at),
    latest_episode_added_at_sort = CASE WHEN item_type = 'SERIES'
        THEN COALESCE(latest_added_at, effective_added_at)
        ELSE NULL END
WHERE recent_added_at IS NULL;

CREATE INDEX idx_catalog_items_recent_sort_added
    ON catalog_items(library_id, recent_added_at DESC, sort_title, item_id);
