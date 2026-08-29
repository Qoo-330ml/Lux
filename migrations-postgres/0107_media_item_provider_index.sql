CREATE TABLE media_item_provider_ids (
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    item_type TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    PRIMARY KEY (media_item_id, provider, provider_id)
);

CREATE INDEX idx_media_item_provider_ids_lookup
    ON media_item_provider_ids(item_type, provider, provider_id, media_item_id);

INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
SELECT mi.id, mi.item_type, lower(providers.key), providers.value
FROM media_items AS mi
CROSS JOIN LATERAL json_each_text(
    CASE
        WHEN mi.provider_ids_json IS NULL THEN '{}'::json
        ELSE mi.provider_ids_json::json
    END
) AS providers
WHERE providers.value IS NOT NULL
ON CONFLICT (media_item_id, provider, provider_id) DO NOTHING;

CREATE OR REPLACE FUNCTION lux_sync_media_item_provider_ids()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_item_provider_ids WHERE media_item_id = NEW.id;
    INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
    SELECT NEW.id, NEW.item_type, lower(providers.key), providers.value
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

CREATE TRIGGER media_item_provider_ids_ai
AFTER INSERT ON media_items
FOR EACH ROW EXECUTE FUNCTION lux_sync_media_item_provider_ids();

CREATE TRIGGER media_item_provider_ids_au
AFTER UPDATE OF item_type, provider_ids_json ON media_items
FOR EACH ROW EXECUTE FUNCTION lux_sync_media_item_provider_ids();
