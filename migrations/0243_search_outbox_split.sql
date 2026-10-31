-- The search reindex stream gets a table of its own. Until now `kind = 'search'` rows shared
-- `catalog_outbox` (and its single id sequence) with the `kind = 'catalog'` read-model events, so a
-- consumer's watermark and its "is there a hole" check lived in different coordinates; the resulting
-- false gap detections re-queued every item in a loop and filled the disk. With one table per stream
-- the sequence belongs to exactly one consumer.
--
-- Written to be re-runnable (IF NOT EXISTS / DROP IF EXISTS).
CREATE TABLE IF NOT EXISTS search_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    item_id TEXT NOT NULL,
    library_id TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS idx_search_outbox_kind ON search_outbox(kind, id);

DROP TRIGGER IF EXISTS media_items_search_ai;
DROP TRIGGER IF EXISTS media_items_search_au;
DROP TRIGGER IF EXISTS media_items_search_ad;
DROP TRIGGER IF EXISTS item_aliases_search_ai;
DROP TRIGGER IF EXISTS item_aliases_search_au;
DROP TRIGGER IF EXISTS item_aliases_search_ad;

CREATE TRIGGER media_items_search_ai
AFTER INSERT ON media_items
BEGIN
    INSERT INTO search_outbox (kind, item_id) VALUES ('search', NEW.id);
END;

CREATE TRIGGER media_items_search_au
AFTER UPDATE OF title, sort_title, original_title ON media_items
WHEN OLD.title IS NOT NEW.title
  OR OLD.sort_title IS NOT NEW.sort_title
  OR OLD.original_title IS NOT NEW.original_title
BEGIN
    INSERT INTO search_outbox (kind, item_id) VALUES ('search', NEW.id);
END;

CREATE TRIGGER media_items_search_ad
AFTER DELETE ON media_items
BEGIN
    INSERT INTO search_outbox (kind, item_id) VALUES ('search', OLD.id);
END;

CREATE TRIGGER item_aliases_search_ai
AFTER INSERT ON item_aliases
BEGIN
    INSERT INTO search_outbox (kind, item_id) VALUES ('search', NEW.item_id);
END;

CREATE TRIGGER item_aliases_search_au
AFTER UPDATE OF alias, alias_normalized, item_id ON item_aliases
WHEN OLD.alias IS NOT NEW.alias
  OR OLD.alias_normalized IS NOT NEW.alias_normalized
  OR OLD.item_id IS NOT NEW.item_id
BEGIN
    INSERT INTO search_outbox (kind, item_id) VALUES ('search', NEW.item_id);
    INSERT INTO search_outbox (kind, item_id)
    SELECT 'search', OLD.item_id WHERE OLD.item_id IS NOT NEW.item_id;
END;

CREATE TRIGGER item_aliases_search_ad
AFTER DELETE ON item_aliases
BEGIN
    INSERT INTO search_outbox (kind, item_id) VALUES ('search', OLD.item_id);
END;

-- The `search_reindex@1` watermark was a position in the old shared sequence and means nothing in
-- the new table; the consumer continues as `search_reindex@2` from zero.
DELETE FROM catalog_consumers WHERE name = 'search_reindex@1';

-- Whatever the old stream still held is superseded by one pass over every item, so the upgrade can
-- neither lose an index update nor depend on the old backlog. The old rows are deleted by the
-- background worker in bounded batches, never inside this migration.
INSERT INTO search_outbox (kind, item_id)
SELECT 'search', id FROM media_items;
