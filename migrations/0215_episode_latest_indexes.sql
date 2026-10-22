-- Home "recently added" resolves the latest episode per series/season. Partial
-- indexes keep that MAX aggregate index-only instead of scanning every episode.
CREATE INDEX idx_media_items_series_episode_latest
    ON media_items(series_id, added_at)
    WHERE item_type = 'EPISODE'
      AND removed_at IS NULL
      AND has_available_source = 1
      AND series_id IS NOT NULL;

CREATE INDEX idx_media_items_parent_episode_latest
    ON media_items(parent_id, added_at)
    WHERE item_type = 'EPISODE'
      AND removed_at IS NULL
      AND has_available_source = 1
      AND parent_id IS NOT NULL;
