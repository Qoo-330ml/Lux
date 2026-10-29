-- The pending-reconciliation query reads NFO JSON from media_item_nfo_cache, so the old partial
-- index (which required media_items.nfo_metadata_json IS NOT NULL) could no longer be chosen and
-- every poll walked the whole media_items primary key.
DROP INDEX IF EXISTS idx_media_items_nfo_collection_reference_pending;

CREATE INDEX idx_media_items_nfo_collection_reference_pending
    ON media_items(id)
    WHERE item_type = 'MOVIE'
      AND removed_at IS NULL
      AND nfo_collection_reference_key IS NULL;
