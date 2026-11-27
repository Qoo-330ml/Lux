-- PostgreSQL counterpart of 0254_index_series_next_people_merge_and_clear_legacy_nfo_json.sql.

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
SELECT id, nfo_metadata_fingerprint, nfo_metadata_json, octet_length(nfo_metadata_json)
FROM media_items
WHERE nfo_metadata_json IS NOT NULL
ON CONFLICT (item_id) DO NOTHING;

DROP TRIGGER IF EXISTS trg_media_items_content_restriction_index_invalidate_nfo ON media_items;

UPDATE media_items
SET nfo_metadata_json = NULL
WHERE nfo_metadata_json IS NOT NULL;

CREATE TRIGGER trg_media_items_content_restriction_index_invalidate_nfo
AFTER UPDATE OF nfo_metadata_json ON media_items
FOR EACH ROW
WHEN (OLD.nfo_metadata_json IS DISTINCT FROM NEW.nfo_metadata_json)
EXECUTE FUNCTION media_content_restriction_index_invalidate_nfo_change();
