-- The search reindex stream gets a table of its own (mirrors SQLite 0243_search_outbox_split.sql).
-- `kind = 'search'` rows used to share `catalog_outbox` and its single sequence with the
-- `kind = 'catalog'` events, which made every consumer watermark ambiguous and let false gap
-- detections re-queue every item in a loop. The statement-level triggers installed by 0239 keep
-- pointing at the same function names; only the function bodies change target.
CREATE TABLE IF NOT EXISTS search_outbox (
    id BIGSERIAL PRIMARY KEY,
    kind TEXT NOT NULL,
    item_id TEXT NOT NULL,
    library_id TEXT,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM now())::BIGINT)
);

CREATE INDEX IF NOT EXISTS idx_search_outbox_kind ON search_outbox(kind, id);

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_items_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO search_outbox (kind, item_id)
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

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_items_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO search_outbox (kind, item_id)
    SELECT 'search', n.id
    FROM new_rows n
    JOIN old_rows o ON o.id = n.id
    WHERE n.title IS DISTINCT FROM o.title
       OR n.sort_title IS DISTINCT FROM o.sort_title
       OR n.original_title IS DISTINCT FROM o.original_title;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_items_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO search_outbox (kind, item_id)
    SELECT 'search', id FROM old_rows;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_aliases_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO search_outbox (kind, item_id)
    SELECT DISTINCT 'search', item_id FROM new_rows;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_aliases_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO search_outbox (kind, item_id)
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

CREATE OR REPLACE FUNCTION lux_enqueue_search_reindex_aliases_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO search_outbox (kind, item_id)
    SELECT DISTINCT 'search', item_id FROM old_rows;
    RETURN NULL;
END;
$$;

-- `search_reindex@1` was a position in the old shared sequence; the consumer continues as
-- `search_reindex@2` from zero.
DELETE FROM catalog_consumers WHERE name = 'search_reindex@1';

-- One pass over every item supersedes whatever the old stream still held. The old rows are deleted
-- by the background worker in bounded batches, never inside this migration.
INSERT INTO search_outbox (kind, item_id)
SELECT 'search', id FROM media_items;
