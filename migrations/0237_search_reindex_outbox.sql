-- Search reindexing moves off the writer's transaction (W4): the triggers only append to
-- `catalog_outbox`, and the `search_reindex` consumer rewrites the FTS row from the authoritative
-- tables. A scan therefore inserts one cheap outbox row per item instead of maintaining the FTS
-- index (plus a `group_concat` alias subquery) inside every write transaction.
--
-- `media_search_map` (rowid assignment) stays: the consumer claims rowids in batches.
DROP TRIGGER IF EXISTS media_items_search_ai;
DROP TRIGGER IF EXISTS media_items_search_au;
DROP TRIGGER IF EXISTS media_items_search_ad;
DROP TRIGGER IF EXISTS item_aliases_search_ai;
DROP TRIGGER IF EXISTS item_aliases_search_au;
DROP TRIGGER IF EXISTS item_aliases_search_ad;

CREATE TRIGGER media_items_search_ai
AFTER INSERT ON media_items
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', NEW.id);
END;

CREATE TRIGGER media_items_search_au
AFTER UPDATE OF title, sort_title, original_title ON media_items
WHEN OLD.title IS NOT NEW.title
  OR OLD.sort_title IS NOT NEW.sort_title
  OR OLD.original_title IS NOT NEW.original_title
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', NEW.id);
END;

CREATE TRIGGER media_items_search_ad
AFTER DELETE ON media_items
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', OLD.id);
END;

CREATE TRIGGER item_aliases_search_ai
AFTER INSERT ON item_aliases
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', NEW.item_id);
END;

CREATE TRIGGER item_aliases_search_au
AFTER UPDATE OF alias, alias_normalized, item_id ON item_aliases
WHEN OLD.alias IS NOT NEW.alias
  OR OLD.alias_normalized IS NOT NEW.alias_normalized
  OR OLD.item_id IS NOT NEW.item_id
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', NEW.item_id);
    INSERT INTO catalog_outbox (kind, item_id)
    SELECT 'search', OLD.item_id WHERE OLD.item_id IS NOT NEW.item_id;
END;

CREATE TRIGGER item_aliases_search_ad
AFTER DELETE ON item_aliases
BEGIN
    INSERT INTO catalog_outbox (kind, item_id) VALUES ('search', OLD.item_id);
END;

-- Items that never had a search row (map entry) get one through the consumer, so an upgrade does
-- not leave a permanently unindexed tail.
INSERT INTO catalog_outbox (kind, item_id)
SELECT 'search', item.id
FROM media_items item
LEFT JOIN media_search_map map ON map.item_id = item.id
WHERE map.item_id IS NULL;
