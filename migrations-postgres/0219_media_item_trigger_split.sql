-- PostgreSQL cannot combine transition tables with a column-specific UPDATE
-- trigger. The statement trigger therefore fired for every media_items update,
-- including availability-only batches. Split the update path into row triggers
-- that only run when the searchable title or provider columns change.
DROP TRIGGER IF EXISTS media_items_search_au ON media_items;

CREATE OR REPLACE FUNCTION lux_refresh_media_item_provider_ids_row()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_item_provider_ids WHERE media_item_id = NEW.id;
    INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
    SELECT NEW.id,
           NEW.item_type,
           lower(providers.key),
           providers.value
    FROM json_each_text(
        CASE
            WHEN NEW.provider_ids_json IS NULL THEN '{}'::json
            ELSE NEW.provider_ids_json::json
        END
    ) AS providers
    WHERE providers.value IS NOT NULL
    ON CONFLICT (media_item_id, provider, provider_id) DO NOTHING;
    RETURN NEW;
END;
$$;

CREATE TRIGGER media_item_provider_ids_au
AFTER UPDATE OF item_type, provider_ids_json ON media_items
FOR EACH ROW
WHEN (
    OLD.item_type IS DISTINCT FROM NEW.item_type
    OR OLD.provider_ids_json IS DISTINCT FROM NEW.provider_ids_json
)
EXECUTE FUNCTION lux_refresh_media_item_provider_ids_row();

CREATE TRIGGER media_items_search_au
AFTER UPDATE OF title, sort_title, original_title ON media_items
FOR EACH ROW
WHEN (
    OLD.title IS DISTINCT FROM NEW.title
    OR OLD.sort_title IS DISTINCT FROM NEW.sort_title
    OR OLD.original_title IS DISTINCT FROM NEW.original_title
)
EXECUTE FUNCTION lux_refresh_media_search_item();
