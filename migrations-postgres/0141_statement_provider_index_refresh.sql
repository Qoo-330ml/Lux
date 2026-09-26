-- Refresh the provider lookup index once per media_items write statement.
-- The original row triggers rebuilt an empty provider set for every scanned item,
-- which made large movie INSERT statements pay one DELETE/INSERT trigger call per row.

DROP TRIGGER IF EXISTS media_item_provider_ids_ai ON media_items;
DROP TRIGGER IF EXISTS media_item_provider_ids_au ON media_items;

CREATE OR REPLACE FUNCTION lux_sync_media_item_provider_ids_insert_stmt()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
    SELECT n.id, n.item_type, lower(providers.key), providers.value
    FROM new_rows n
    CROSS JOIN LATERAL json_each_text(
        CASE
            WHEN n.provider_ids_json IS NULL THEN '{}'::json
            ELSE n.provider_ids_json::json
        END
    ) AS providers
    WHERE providers.value IS NOT NULL
    ON CONFLICT (media_item_id, provider, provider_id) DO NOTHING;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_item_provider_ids_ai
AFTER INSERT ON media_items
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_sync_media_item_provider_ids_insert_stmt();

CREATE OR REPLACE FUNCTION lux_sync_media_item_provider_ids_update_stmt()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_item_provider_ids provider
    USING old_rows old_item
    JOIN new_rows new_item ON new_item.id = old_item.id
    WHERE provider.media_item_id = old_item.id
      AND (
          old_item.item_type IS DISTINCT FROM new_item.item_type
          OR old_item.provider_ids_json IS DISTINCT FROM new_item.provider_ids_json
      );

    INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
    SELECT n.id, n.item_type, lower(providers.key), providers.value
    FROM new_rows n
    JOIN old_rows o ON o.id = n.id
    CROSS JOIN LATERAL json_each_text(
        CASE
            WHEN n.provider_ids_json IS NULL THEN '{}'::json
            ELSE n.provider_ids_json::json
        END
    ) AS providers
    WHERE providers.value IS NOT NULL
      AND (
          o.item_type IS DISTINCT FROM n.item_type
          OR o.provider_ids_json IS DISTINCT FROM n.provider_ids_json
      )
    ON CONFLICT (media_item_id, provider, provider_id) DO NOTHING;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_item_provider_ids_au
AFTER UPDATE ON media_items
REFERENCING NEW TABLE AS new_rows OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_sync_media_item_provider_ids_update_stmt();
