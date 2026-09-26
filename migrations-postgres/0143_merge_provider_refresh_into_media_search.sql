-- Refresh the provider lookup together with the existing media search
-- statement-level trigger.  A scan INSERT/UPDATE should pay one transition
-- table trigger invocation instead of a separate provider trigger invocation
-- for every media_items statement.

-- PostgreSQL catalog search uses ILIKE with a contains pattern and orders by
-- media_items.sort_title, so these per-row B-trees are not used by the query
-- path but are maintained for every media_search row during a full scan.
DROP INDEX IF EXISTS idx_media_search_title;
DROP INDEX IF EXISTS idx_media_search_sort_title;

DROP TRIGGER IF EXISTS media_item_provider_ids_ai ON media_items;
DROP TRIGGER IF EXISTS media_item_provider_ids_au ON media_items;

CREATE OR REPLACE FUNCTION lux_refresh_media_search_items_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
    SELECT n.id,
           n.title,
           n.sort_title,
           COALESCE(n.original_title, ''),
           ''
    FROM new_rows n;

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

CREATE OR REPLACE FUNCTION lux_refresh_media_search_items_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
    SELECT n.id,
           n.title,
           n.sort_title,
           COALESCE(n.original_title, ''),
           COALESCE(existing.aliases, '')
    FROM new_rows n
    JOIN old_rows o ON o.id = n.id
    LEFT JOIN media_search existing ON existing.item_id = n.id
    WHERE n.title IS DISTINCT FROM o.title
       OR n.sort_title IS DISTINCT FROM o.sort_title
       OR n.original_title IS DISTINCT FROM o.original_title
       OR existing.item_id IS NULL
    ON CONFLICT (item_id) DO UPDATE SET
        title = EXCLUDED.title,
        sort_title = EXCLUDED.sort_title,
        original_title = EXCLUDED.original_title,
        aliases = EXCLUDED.aliases;

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
