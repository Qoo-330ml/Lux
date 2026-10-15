CREATE INDEX idx_media_items_nfo_collection_reference_pending
    ON media_items(id)
    WHERE item_type = 'MOVIE'
      AND removed_at IS NULL
      AND nfo_metadata_json IS NOT NULL
      AND nfo_collection_reference_key IS NULL;
