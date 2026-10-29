-- W6: PostgreSQL incremental maintenance for `catalog_items`, mirroring the SQLite contract
-- (0238_catalog_items_incremental_outbox.sql). Triggers only append `kind = 'catalog'` rows to
-- `catalog_outbox`; the `catalog_items_incremental@1` consumer re-derives the touched rows.
-- Statement-level triggers keep one outbox insert per write statement instead of per row.
DROP TRIGGER IF EXISTS media_items_catalog_ai ON media_items;
DROP TRIGGER IF EXISTS media_items_catalog_au ON media_items;
DROP TRIGGER IF EXISTS media_items_catalog_ad ON media_items;
DROP TRIGGER IF EXISTS media_sources_catalog_ai ON media_sources;
DROP TRIGGER IF EXISTS media_sources_catalog_au ON media_sources;
DROP TRIGGER IF EXISTS media_sources_catalog_ad ON media_sources;
DROP TRIGGER IF EXISTS item_images_catalog_ai ON item_images;
DROP TRIGGER IF EXISTS item_images_catalog_au ON item_images;
DROP TRIGGER IF EXISTS item_images_catalog_ad ON item_images;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_ai ON media_content_restriction_index;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_au ON media_content_restriction_index;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_ad ON media_content_restriction_index;

DROP FUNCTION IF EXISTS lux_enqueue_catalog_items_insert_stmt();
DROP FUNCTION IF EXISTS lux_enqueue_catalog_items_update_stmt();
DROP FUNCTION IF EXISTS lux_enqueue_catalog_items_delete_stmt();
DROP FUNCTION IF EXISTS lux_enqueue_catalog_children_insert_stmt();
DROP FUNCTION IF EXISTS lux_enqueue_catalog_children_update_stmt();
DROP FUNCTION IF EXISTS lux_enqueue_catalog_children_delete_stmt();

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_items_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', id FROM new_rows;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', parent_id FROM new_rows WHERE parent_id IS NOT NULL;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', series_id FROM new_rows
    WHERE series_id IS NOT NULL AND series_id <> id;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_items_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', id FROM new_rows;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', candidate.value
    FROM (
        SELECT n.parent_id AS value, n.id AS item_id FROM new_rows n
        UNION ALL
        SELECT o.parent_id, o.id FROM old_rows o
        UNION ALL
        SELECT n.series_id, n.id FROM new_rows n
        UNION ALL
        SELECT o.series_id, o.id FROM old_rows o
    ) candidate
    WHERE candidate.value IS NOT NULL
      AND candidate.value <> candidate.item_id;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_items_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', id FROM old_rows;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', parent_id FROM old_rows WHERE parent_id IS NOT NULL;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', series_id FROM old_rows
    WHERE series_id IS NOT NULL AND series_id <> id;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_children_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', item_id FROM new_rows WHERE item_id IS NOT NULL;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_children_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', item_id
    FROM (
        SELECT item_id FROM new_rows
        UNION ALL
        SELECT item_id FROM old_rows
    ) candidate
    WHERE item_id IS NOT NULL;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_children_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', item_id FROM old_rows WHERE item_id IS NOT NULL;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_items_catalog_ai
AFTER INSERT ON media_items
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_items_insert_stmt();

CREATE TRIGGER media_items_catalog_au
AFTER UPDATE ON media_items
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_items_update_stmt();

CREATE TRIGGER media_items_catalog_ad
AFTER DELETE ON media_items
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_items_delete_stmt();

CREATE TRIGGER media_sources_catalog_ai
AFTER INSERT ON media_sources
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_insert_stmt();

CREATE TRIGGER media_sources_catalog_au
AFTER UPDATE ON media_sources
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_update_stmt();

CREATE TRIGGER media_sources_catalog_ad
AFTER DELETE ON media_sources
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_delete_stmt();

CREATE TRIGGER item_images_catalog_ai
AFTER INSERT ON item_images
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_insert_stmt();

CREATE TRIGGER item_images_catalog_au
AFTER UPDATE ON item_images
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_update_stmt();

CREATE TRIGGER item_images_catalog_ad
AFTER DELETE ON item_images
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_delete_stmt();

CREATE TRIGGER media_content_restriction_catalog_ai
AFTER INSERT ON media_content_restriction_index
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_insert_stmt();

CREATE TRIGGER media_content_restriction_catalog_au
AFTER UPDATE ON media_content_restriction_index
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_update_stmt();

CREATE TRIGGER media_content_restriction_catalog_ad
AFTER DELETE ON media_content_restriction_index
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_children_delete_stmt();

-- Re-derive every existing row once on upgrade; the consumer is idempotent.
INSERT INTO catalog_outbox (kind, item_id)
SELECT 'catalog', id FROM media_items;
