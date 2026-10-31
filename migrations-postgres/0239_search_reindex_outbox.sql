-- W4: PostgreSQL search indexing moves off the writer's transaction, mirroring the SQLite contract
-- (0237_search_reindex_outbox.sql). Triggers only append `kind = 'search'` rows to catalog_outbox;
-- the `search_reindex@1` consumer rewrites media_search from the authoritative tables in batches.
-- Statement-level triggers keep the old amortisation: one outbox insert per write statement.
DROP TRIGGER IF EXISTS media_items_search_ai ON media_items;
DROP TRIGGER IF EXISTS media_items_search_au ON media_items;
DROP TRIGGER IF EXISTS media_items_search_ad ON media_items;
DROP TRIGGER IF EXISTS item_aliases_search_ai ON item_aliases;
DROP TRIGGER IF EXISTS item_aliases_search_au ON item_aliases;
DROP TRIGGER IF EXISTS item_aliases_search_ad ON item_aliases;

DROP FUNCTION IF EXISTS lux_refresh_media_search_items_insert_stmt();
DROP FUNCTION IF EXISTS lux_refresh_media_search_items_update_stmt();
DROP FUNCTION IF EXISTS lux_refresh_media_search_items_delete_stmt();
DROP FUNCTION IF EXISTS lux_refresh_media_search_aliases_insert_stmt();
DROP FUNCTION IF EXISTS lux_refresh_media_search_aliases_update_stmt();
DROP FUNCTION IF EXISTS lux_refresh_media_search_aliases_delete_stmt();

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_items_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'search', id FROM new_rows;
    -- The provider index stays synchronous (W4): it is read by merge/archive decisions in the
    -- same flow, so it cannot wait for the consumer. This mirrors 0143, which merged it here.
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

CREATE TRIGGER media_items_search_ai
AFTER INSERT ON media_items
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_search_reindex_items_insert_stmt();

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_items_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'search', n.id
    FROM new_rows n
    JOIN old_rows o ON o.id = n.id
    WHERE n.title IS DISTINCT FROM o.title
       OR n.sort_title IS DISTINCT FROM o.sort_title
       OR n.original_title IS DISTINCT FROM o.original_title;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_items_search_au
AFTER UPDATE ON media_items
REFERENCING NEW TABLE AS new_rows
             OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_search_reindex_items_update_stmt();

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_items_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'search', id FROM old_rows;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_items_search_ad
AFTER DELETE ON media_items
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_search_reindex_items_delete_stmt();

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_aliases_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'search', item_id FROM new_rows;
    RETURN NULL;
END;
$$;

CREATE TRIGGER item_aliases_search_ai
AFTER INSERT ON item_aliases
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_search_reindex_aliases_insert_stmt();

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_aliases_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'search', affected.item_id
    FROM (
        SELECT o.item_id
        FROM old_rows o
        JOIN new_rows n ON n.id = o.id
        WHERE o.alias IS DISTINCT FROM n.alias
           OR o.alias_normalized IS DISTINCT FROM n.alias_normalized
           OR o.item_id IS DISTINCT FROM n.item_id
        UNION
        SELECT n.item_id
        FROM old_rows o
        JOIN new_rows n ON n.id = o.id
        WHERE o.alias IS DISTINCT FROM n.alias
           OR o.alias_normalized IS DISTINCT FROM n.alias_normalized
           OR o.item_id IS DISTINCT FROM n.item_id
    ) affected;
    RETURN NULL;
END;
$$;

CREATE TRIGGER item_aliases_search_au
AFTER UPDATE ON item_aliases
REFERENCING OLD TABLE AS old_rows
             NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_search_reindex_aliases_update_stmt();

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_aliases_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'search', item_id FROM old_rows;
    RETURN NULL;
END;
$$;

CREATE TRIGGER item_aliases_search_ad
AFTER DELETE ON item_aliases
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_search_reindex_aliases_delete_stmt();

-- Items without a search row get queued once, so the upgrade does not leave an unindexed tail.
INSERT INTO catalog_outbox (kind, item_id)
SELECT 'search', item.id
FROM media_items item
LEFT JOIN media_search search_row ON search_row.item_id = item.id
WHERE search_row.item_id IS NULL;
