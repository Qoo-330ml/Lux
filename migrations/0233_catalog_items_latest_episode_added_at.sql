-- Latest available-episode timestamp for a series, mirroring the home "latest by library" rail.
-- `latest_child_added_at` also counts seasons and removed children, so it cannot drive that order.
ALTER TABLE catalog_items ADD COLUMN latest_episode_added_at INTEGER;
