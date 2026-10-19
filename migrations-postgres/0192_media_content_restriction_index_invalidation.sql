CREATE FUNCTION media_content_restriction_index_invalidate_nfo_change()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_content_restriction_index WHERE item_id = NEW.id;
    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_media_items_content_restriction_index_invalidate_nfo
AFTER UPDATE OF nfo_metadata_json ON media_items
FOR EACH ROW
WHEN (OLD.nfo_metadata_json IS DISTINCT FROM NEW.nfo_metadata_json)
EXECUTE FUNCTION media_content_restriction_index_invalidate_nfo_change();
