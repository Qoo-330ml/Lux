-- Incremental maintenance for the derived read model.
--
-- Authoritative writes only append one cheap `kind = 'catalog'` row to `catalog_outbox`; the
-- `catalog_items_incremental@1` consumer re-derives exactly the touched rows from the authoritative
-- tables. Without this a scan stays invisible to every read-model path until the next full rebuild.
--
-- Parent and series rows are enqueued as well: `season_count`, `episode_count`,
-- `latest_child_added_at` and `latest_episode_added_at` of a container are inputs of its own
-- derived row, so a child insert/update/delete must refresh the container too.
--
-- The update triggers intentionally fire on every update instead of a column list: `media_items`
-- carries most of the derived inputs, and a missed column would leave a permanently stale row.

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
AFTER UPDATE ON media_items
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', NEW.parent_id
    WHERE NEW.parent_id IS NOT NULL AND NEW.parent_id IS NOT NEW.id;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.parent_id
    WHERE OLD.parent_id IS NOT NULL
      AND OLD.parent_id IS NOT OLD.id
      AND OLD.parent_id IS NOT NEW.id;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', NEW.series_id
    WHERE NEW.series_id IS NOT NULL AND NEW.series_id IS NOT NEW.id;
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.series_id
    WHERE OLD.series_id IS NOT NULL
      AND OLD.series_id IS NOT OLD.id
      AND OLD.series_id IS NOT NEW.id;
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

CREATE TRIGGER media_sources_catalog_ai
AFTER INSERT ON media_sources
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.item_id);
END;

CREATE TRIGGER media_sources_catalog_au
AFTER UPDATE ON media_sources
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.item_id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.item_id WHERE OLD.item_id IS NOT NEW.item_id;
END;

CREATE TRIGGER media_sources_catalog_ad
AFTER DELETE ON media_sources
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', OLD.item_id);
END;

CREATE TRIGGER item_images_catalog_ai
AFTER INSERT ON item_images
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.item_id);
END;

CREATE TRIGGER item_images_catalog_au
AFTER UPDATE ON item_images
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.item_id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.item_id WHERE OLD.item_id IS NOT NEW.item_id;
END;

CREATE TRIGGER item_images_catalog_ad
AFTER DELETE ON item_images
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', OLD.item_id);
END;

CREATE TRIGGER media_content_restriction_catalog_ai
AFTER INSERT ON media_content_restriction_index
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.item_id);
END;

CREATE TRIGGER media_content_restriction_catalog_au
AFTER UPDATE ON media_content_restriction_index
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', NEW.item_id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'catalog', OLD.item_id WHERE OLD.item_id IS NOT NEW.item_id;
END;

CREATE TRIGGER media_content_restriction_catalog_ad
AFTER DELETE ON media_content_restriction_index
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('catalog', OLD.item_id);
END;

-- Re-derive every existing row once on upgrade: rows written before these triggers may already be
-- stale, and the consumer is idempotent.
INSERT INTO catalog_outbox (kind, item_id)
SELECT 'catalog', id FROM media_items;
