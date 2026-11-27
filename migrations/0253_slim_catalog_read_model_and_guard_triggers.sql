-- Slim the derived `catalog_items` read model and guard update/availability triggers.
--
-- 1. Drop `catalog_item_stats` (zero production readers; rebuilt via full-table GROUP BY scans).
-- 2. Drop `idx_catalog_items_recent` (strict left prefix of `idx_catalog_items_recent_sort`) and
--    `idx_catalog_items_restriction` (unread column).
-- 3. Drop the 11 columns of `catalog_items` that no production query reads (`updated_at` is read
--    directly from the `media_items` primary-key join in `read_model_rows_for_ids`).
-- 4. Retire `media_sources_catalog_*` and `media_content_restriction_catalog_*` outbox triggers
--    (neither table feeds any surviving `catalog_items` column; `has_available_source` lives on
--    `media_items`), add column lists + `WHEN` guards to `media_items_catalog_au` and
--    `item_images_catalog_*`, and only fan out container refreshes when hierarchy/availability/add
--    timestamps actually change.
-- 5. Guard SQLite `has_available_source` update/delete triggers so no-op recalculations do not
--    dirty `media_items`.

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

DROP TRIGGER IF EXISTS media_items_catalog_ai;
DROP TRIGGER IF EXISTS media_items_catalog_au;
DROP TRIGGER IF EXISTS media_items_catalog_ad;
DROP TRIGGER IF EXISTS media_sources_catalog_ai;
DROP TRIGGER IF EXISTS media_sources_catalog_au;
DROP TRIGGER IF EXISTS media_sources_catalog_ad;
DROP TRIGGER IF EXISTS item_images_catalog_ai;
DROP TRIGGER IF EXISTS item_images_catalog_au;
DROP TRIGGER IF EXISTS item_images_catalog_ad;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_ai;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_au;
DROP TRIGGER IF EXISTS media_content_restriction_catalog_ad;

CREATE TRIGGER media_items_catalog_ai
AFTER INSERT ON media_items
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', NEW.parent_id
    WHERE NEW.parent_id IS NOT NULL AND NEW.parent_id IS NOT NEW.id;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', NEW.series_id
    WHERE NEW.series_id IS NOT NULL AND NEW.series_id IS NOT NEW.id;
END;

CREATE TRIGGER media_items_catalog_au
AFTER UPDATE OF
    library_id, item_type, parent_id, series_id, season_number, episode_number,
    title, sort_title, original_title, production_year, rating, runtime_ticks,
    added_at, removed_at, merged_into_item_id, has_available_source
ON media_items
WHEN OLD.library_id IS NOT NEW.library_id
  OR OLD.item_type IS NOT NEW.item_type
  OR OLD.parent_id IS NOT NEW.parent_id
  OR OLD.series_id IS NOT NEW.series_id
  OR OLD.season_number IS NOT NEW.season_number
  OR OLD.episode_number IS NOT NEW.episode_number
  OR OLD.title IS NOT NEW.title
  OR OLD.sort_title IS NOT NEW.sort_title
  OR OLD.original_title IS NOT NEW.original_title
  OR OLD.production_year IS NOT NEW.production_year
  OR OLD.rating IS NOT NEW.rating
  OR OLD.runtime_ticks IS NOT NEW.runtime_ticks
  OR OLD.added_at IS NOT NEW.added_at
  OR OLD.removed_at IS NOT NEW.removed_at
  OR OLD.merged_into_item_id IS NOT NEW.merged_into_item_id
  OR OLD.has_available_source IS NOT NEW.has_available_source
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', NEW.parent_id
    WHERE NEW.parent_id IS NOT NULL
      AND NEW.parent_id IS NOT NEW.id
      AND (OLD.item_type IS NOT NEW.item_type
           OR OLD.parent_id IS NOT NEW.parent_id
           OR OLD.series_id IS NOT NEW.series_id
           OR OLD.added_at IS NOT NEW.added_at
           OR OLD.removed_at IS NOT NEW.removed_at
           OR OLD.merged_into_item_id IS NOT NEW.merged_into_item_id
           OR OLD.has_available_source IS NOT NEW.has_available_source);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.parent_id
    WHERE OLD.parent_id IS NOT NULL
      AND OLD.parent_id IS NOT OLD.id
      AND OLD.parent_id IS NOT NEW.parent_id;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', NEW.series_id
    WHERE NEW.series_id IS NOT NULL
      AND NEW.series_id IS NOT NEW.id
      AND (OLD.item_type IS NOT NEW.item_type
           OR OLD.parent_id IS NOT NEW.parent_id
           OR OLD.series_id IS NOT NEW.series_id
           OR OLD.added_at IS NOT NEW.added_at
           OR OLD.removed_at IS NOT NEW.removed_at
           OR OLD.merged_into_item_id IS NOT NEW.merged_into_item_id
           OR OLD.has_available_source IS NOT NEW.has_available_source);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.series_id
    WHERE OLD.series_id IS NOT NULL
      AND OLD.series_id IS NOT OLD.id
      AND OLD.series_id IS NOT NEW.series_id;
END;

CREATE TRIGGER media_items_catalog_ad
AFTER DELETE ON media_items
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', OLD.id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.parent_id
    WHERE OLD.parent_id IS NOT NULL AND OLD.parent_id IS NOT OLD.id;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.series_id
    WHERE OLD.series_id IS NOT NULL AND OLD.series_id IS NOT OLD.id;
END;

CREATE TRIGGER item_images_catalog_ai
AFTER INSERT ON item_images
WHEN NEW.image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO')
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.item_id);
END;

CREATE TRIGGER item_images_catalog_au
AFTER UPDATE OF id, item_id, image_type, image_index ON item_images
WHEN (OLD.id IS NOT NEW.id
      OR OLD.item_id IS NOT NEW.item_id
      OR OLD.image_type IS NOT NEW.image_type
      OR OLD.image_index IS NOT NEW.image_index)
 AND (OLD.image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO')
      OR NEW.image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO'))
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.item_id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.item_id WHERE OLD.item_id IS NOT NEW.item_id;
END;

CREATE TRIGGER item_images_catalog_ad
AFTER DELETE ON item_images
WHEN OLD.image_type IN ('POSTER', 'FANART', 'THUMB', 'LOGO')
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', OLD.item_id);
END;

DROP TRIGGER IF EXISTS trg_media_sources_availability_update;
CREATE TRIGGER trg_media_sources_availability_update
AFTER UPDATE OF item_id, filesystem_entry_id ON media_sources
WHEN OLD.item_id IS NOT NEW.item_id
  OR OLD.filesystem_entry_id IS NOT NEW.filesystem_entry_id
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id IN (OLD.item_id, NEW.item_id)
      AND has_available_source IS NOT EXISTS (
          SELECT 1
          FROM media_sources ms
          JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
          WHERE ms.item_id = media_items.id
            AND fe.is_missing = 0
      );
END;

DROP TRIGGER IF EXISTS trg_media_sources_availability_delete;
CREATE TRIGGER trg_media_sources_availability_delete
AFTER DELETE ON media_sources
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id = OLD.item_id
      AND has_available_source IS NOT EXISTS (
          SELECT 1
          FROM media_sources ms
          JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
          WHERE ms.item_id = media_items.id
            AND fe.is_missing = 0
      );
END;

DROP TRIGGER IF EXISTS trg_filesystem_entries_availability_update;
CREATE TRIGGER trg_filesystem_entries_availability_update
AFTER UPDATE OF is_missing ON filesystem_entries
WHEN OLD.is_missing <> NEW.is_missing
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id IN (
        SELECT item_id
        FROM media_sources
        WHERE filesystem_entry_id = NEW.id
    )
      AND has_available_source IS NOT EXISTS (
          SELECT 1
          FROM media_sources ms
          JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
          WHERE ms.item_id = media_items.id
            AND fe.is_missing = 0
      );
END;
