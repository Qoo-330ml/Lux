CREATE TRIGGER trg_media_items_content_restriction_index_invalidate_nfo
AFTER UPDATE OF nfo_metadata_json ON media_items
WHEN OLD.nfo_metadata_json IS NOT NEW.nfo_metadata_json
BEGIN
    DELETE FROM media_content_restriction_index WHERE item_id = NEW.id;
END;
