-- Lets the retention purge of soft-deleted media items find expired rows without
-- scanning the whole catalog. Only removed rows are indexed, so the index stays small.
CREATE INDEX IF NOT EXISTS idx_media_items_removed_at
    ON media_items(removed_at)
    WHERE removed_at IS NOT NULL;
