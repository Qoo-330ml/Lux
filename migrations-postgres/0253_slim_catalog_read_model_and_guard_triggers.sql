-- PostgreSQL counterpart of 0253_slim_catalog_read_model_and_guard_triggers.sql.
--
-- Drops `catalog_item_stats`, redundant/unused `catalog_items` indexes and the 11 unread columns,
-- retires `media_sources_catalog_*` and `media_content_restriction_catalog_*` outbox triggers, and
-- narrows statement-level `media_items` and `item_images` outbox functions so unrelated updates do
-- not append rows to `catalog_outbox`.

DROP TABLE IF EXISTS catalog_item_stats;
DROP INDEX IF EXISTS idx_catalog_items_recent;
DROP INDEX IF EXISTS idx_catalog_items_restriction;

ALTER TABLE catalog_items DROP COLUMN updated_at;
ALTER TABLE catalog_items DROP COLUMN default_source_id;
ALTER TABLE catalog_items DROP COLUMN season_count;
ALTER TABLE catalog_items DROP COLUMN episode_count;
ALTER TABLE catalog_items DROP COLUMN latest_child_added_at;
ALTER TABLE catalog_items DROP COLUMN latest_episode_added_at;
ALTER TABLE catalog_items DROP COLUMN latest_episode_added_at_sort;
ALTER TABLE catalog_items DROP COLUMN restriction_code;
ALTER TABLE catalog_items DROP COLUMN metadata_state;
ALTER TABLE catalog_items DROP COLUMN nfo_fingerprint;
ALTER TABLE catalog_items DROP COLUMN applied_seq;

DROP TRIGGER IF EXISTS media_items_catalog_au ON media_items;
DROP TRIGGER IF EXISTS media_sources_catalog_ai ON media_sources;
DROP TRIGGER IF EXISTS media_sources_catalog_au ON media_sources;
DROP TRIGGER IF EXISTS media_sources_catalog_ad ON media_sources;
DROP TRIGGER IF EXISTS item_images_catalog_ai ON item_images;
DROP TRIGGER IF EXISTS item_images_catalog_au ON item_images;
DROP TRIGGER IF EXISTS item_images_catalog_ad ON item_images;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_ai ON media_content_restriction_index;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_au ON media_content_restriction_index;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_ad ON media_content_restriction_index;

DROP FUNCTION IF EXISTS lux_enqueue_catalog_children_insert_stmt();
DROP FUNCTION IF EXISTS lux_enqueue_catalog_children_update_stmt();
DROP FUNCTION IF EXISTS lux_enqueue_catalog_children_delete_stmt();

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_items_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', COALESCE(n.id, o.id)
    FROM old_rows o
    FULL OUTER JOIN new_rows n ON n.id = o.id
    WHERE o.id IS NULL
       OR n.id IS NULL
       OR o.library_id IS DISTINCT FROM n.library_id
       OR o.item_type IS DISTINCT FROM n.item_type
       OR o.parent_id IS DISTINCT FROM n.parent_id
       OR o.series_id IS DISTINCT FROM n.series_id
       OR o.season_number IS DISTINCT FROM n.season_number
       OR o.episode_number IS DISTINCT FROM n.episode_number
       OR o.title IS DISTINCT FROM n.title
       OR o.sort_title IS DISTINCT FROM n.sort_title
       OR o.original_title IS DISTINCT FROM n.original_title
       OR o.production_year IS DISTINCT FROM n.production_year
       OR o.rating IS DISTINCT FROM n.rating
       OR o.runtime_ticks IS DISTINCT FROM n.runtime_ticks
       OR o.added_at IS DISTINCT FROM n.added_at
       OR o.removed_at IS DISTINCT FROM n.removed_at
       OR o.merged_into_item_id IS DISTINCT FROM n.merged_into_item_id
       OR o.has_available_source IS DISTINCT FROM n.has_available_source;

    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', candidate.container_id
    FROM (
        SELECT n.parent_id AS container_id, n.id AS item_id
        FROM old_rows o
        FULL OUTER JOIN new_rows n ON n.id = o.id
        WHERE n.parent_id IS NOT NULL
          AND (o.id IS NULL
               OR o.item_type IS DISTINCT FROM n.item_type
               OR o.parent_id IS DISTINCT FROM n.parent_id
               OR o.series_id IS DISTINCT FROM n.series_id
               OR o.added_at IS DISTINCT FROM n.added_at
               OR o.removed_at IS DISTINCT FROM n.removed_at
               OR o.merged_into_item_id IS DISTINCT FROM n.merged_into_item_id
               OR o.has_available_source IS DISTINCT FROM n.has_available_source)
        UNION ALL
        SELECT o.parent_id, o.id
        FROM old_rows o
        FULL OUTER JOIN new_rows n ON n.id = o.id
        WHERE o.parent_id IS NOT NULL
          AND (n.id IS NULL OR o.parent_id IS DISTINCT FROM n.parent_id)
        UNION ALL
        SELECT n.series_id, n.id
        FROM old_rows o
        FULL OUTER JOIN new_rows n ON n.id = o.id
        WHERE n.series_id IS NOT NULL
          AND (o.id IS NULL
               OR o.item_type IS DISTINCT FROM n.item_type
               OR o.parent_id IS DISTINCT FROM n.parent_id
               OR o.series_id IS DISTINCT FROM n.series_id
               OR o.added_at IS DISTINCT FROM n.added_at
               OR o.removed_at IS DISTINCT FROM n.removed_at
               OR o.merged_into_item_id IS DISTINCT FROM n.merged_into_item_id
               OR o.has_available_source IS DISTINCT FROM n.has_available_source)
        UNION ALL
        SELECT o.series_id, o.id
        FROM old_rows o
        FULL OUTER JOIN new_rows n ON n.id = o.id
        WHERE o.series_id IS NOT NULL
          AND (n.id IS NULL OR o.series_id IS DISTINCT FROM n.series_id)
    ) candidate
    WHERE candidate.container_id IS NOT NULL
      AND candidate.container_id <> candidate.item_id;

    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_images_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', item_id
    FROM new_rows
    WHERE item_id IS NOT NULL
      AND image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO');
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_images_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', candidate.item_id
    FROM (
        SELECT n.item_id
        FROM old_rows o
        FULL OUTER JOIN new_rows n ON n.id = o.id
        WHERE n.item_id IS NOT NULL
          AND (o.id IS NULL
               OR o.item_id IS DISTINCT FROM n.item_id
               OR o.image_type IS DISTINCT FROM n.image_type
               OR o.image_index IS DISTINCT FROM n.image_index)
          AND (o.image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO')
               OR n.image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO'))
        UNION ALL
        SELECT o.item_id
        FROM old_rows o
        FULL OUTER JOIN new_rows n ON n.id = o.id
        WHERE o.item_id IS NOT NULL
          AND (n.id IS NULL OR o.item_id IS DISTINCT FROM n.item_id)
          AND (o.image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO')
               OR n.image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO'))
    ) candidate;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_enqueue_catalog_images_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT DISTINCT 'catalog', item_id
    FROM old_rows
    WHERE item_id IS NOT NULL
      AND image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO');
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_items_catalog_au
AFTER UPDATE ON media_items
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_items_update_stmt();

CREATE TRIGGER item_images_catalog_ai
AFTER INSERT ON item_images
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_images_insert_stmt();

CREATE TRIGGER item_images_catalog_au
AFTER UPDATE ON item_images
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_images_update_stmt();

CREATE TRIGGER item_images_catalog_ad
AFTER DELETE ON item_images
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_enqueue_catalog_images_delete_stmt();
