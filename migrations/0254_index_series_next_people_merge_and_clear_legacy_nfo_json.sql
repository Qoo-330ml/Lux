-- Index `user_series_next` foreign keys and `people` merge columns, and clear duplicated inline
-- `media_items.nfo_metadata_json` values that were moved to `media_item_nfo_cache` in 0228.

CREATE INDEX IF NOT EXISTS idx_user_series_next_series
    ON user_series_next(series_id);
CREATE INDEX IF NOT EXISTS idx_user_series_next_item
    ON user_series_next(next_item_id);

CREATE INDEX IF NOT EXISTS idx_person_credits_lux_person_id
    ON person_credits(lux_person_id)
    WHERE lux_person_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_user_person_state_person
    ON user_person_state(person_id);
CREATE INDEX IF NOT EXISTS idx_emby_migration_person_favorites_person
    ON emby_migration_person_favorites(lux_person_id);
CREATE INDEX IF NOT EXISTS idx_person_match_candidates_previous
    ON person_match_candidates(previous_person_id)
    WHERE previous_person_id IS NOT NULL;

INSERT INTO media_item_nfo_cache (item_id, fingerprint, json, bytes)
SELECT id, nfo_metadata_fingerprint, nfo_metadata_json,
       length(CAST(nfo_metadata_json AS BLOB))
FROM media_items
WHERE nfo_metadata_json IS NOT NULL
ON CONFLICT(item_id) DO NOTHING;

-- Pause the legacy inline-column invalidation trigger while clearing the redundant copy so already
-- built `media_content_restriction_index` rows stay intact across upgrades.
DROP TRIGGER IF EXISTS trg_media_items_content_restriction_index_invalidate_nfo;

UPDATE media_items
SET nfo_metadata_json = NULL
WHERE nfo_metadata_json IS NOT NULL;

CREATE TRIGGER trg_media_items_content_restriction_index_invalidate_nfo
AFTER UPDATE OF nfo_metadata_json ON media_items
WHEN OLD.nfo_metadata_json IS NOT NEW.nfo_metadata_json
BEGIN
    DELETE FROM media_content_restriction_index WHERE item_id = NEW.id;
    INSERT INTO media_content_restriction_index_queue (item_id)
    VALUES (NEW.id)
    ON CONFLICT(item_id) DO UPDATE SET queued_at = unixepoch();
END;
