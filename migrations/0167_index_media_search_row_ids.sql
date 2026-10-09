-- FTS5 does not index UNINDEXED columns, so deleting by item_id scans every
-- search row. Keep the FTS rowid in an ordinary indexed table instead.
CREATE TABLE media_search_map (
    fts_rowid INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id TEXT NOT NULL UNIQUE
);

INSERT INTO media_search_map (fts_rowid, item_id)
SELECT rowid, item_id
FROM media_search
ORDER BY rowid;

DROP TRIGGER IF EXISTS media_items_search_ai;
DROP TRIGGER IF EXISTS media_items_search_au;
DROP TRIGGER IF EXISTS media_items_search_ad;
DROP TRIGGER IF EXISTS item_aliases_search_ai;
DROP TRIGGER IF EXISTS item_aliases_search_au;
DROP TRIGGER IF EXISTS item_aliases_search_ad;
DROP TABLE media_search;

CREATE VIRTUAL TABLE media_search USING fts5(
    item_id UNINDEXED,
    title,
    sort_title,
    original_title,
    aliases,
    columnsize=0
);

INSERT INTO media_search (rowid, item_id, title, sort_title, original_title, aliases)
SELECT map.fts_rowid,
       media_items.id,
       media_items.title,
       CASE
           WHEN media_items.sort_title = media_items.title COLLATE NOCASE THEN ''
           ELSE media_items.sort_title
       END,
       COALESCE(media_items.original_title, ''),
       COALESCE((SELECT group_concat(alias, ' ')
                 FROM item_aliases
                 WHERE item_id = media_items.id), '')
FROM media_items
JOIN media_search_map map ON map.item_id = media_items.id;

CREATE TRIGGER media_items_search_ai AFTER INSERT ON media_items BEGIN
    INSERT INTO media_search_map (item_id) VALUES (NEW.id);
    INSERT INTO media_search (rowid, item_id, title, sort_title, original_title, aliases)
    SELECT map.fts_rowid,
           NEW.id,
           NEW.title,
           CASE WHEN NEW.sort_title = NEW.title COLLATE NOCASE THEN '' ELSE NEW.sort_title END,
           COALESCE(NEW.original_title, ''),
           ''
    FROM media_search_map map
    WHERE map.item_id = NEW.id;
END;

CREATE TRIGGER media_items_search_au AFTER UPDATE OF title, sort_title, original_title ON media_items
WHEN OLD.title IS NOT NEW.title
  OR OLD.sort_title IS NOT NEW.sort_title
  OR OLD.original_title IS NOT NEW.original_title
BEGIN
    DELETE FROM media_search
    WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = OLD.id);
    INSERT INTO media_search (rowid, item_id, title, sort_title, original_title, aliases)
    SELECT map.fts_rowid,
           NEW.id,
           NEW.title,
           CASE WHEN NEW.sort_title = NEW.title COLLATE NOCASE THEN '' ELSE NEW.sort_title END,
           COALESCE(NEW.original_title, ''),
           COALESCE((SELECT group_concat(alias, ' ')
                     FROM item_aliases
                     WHERE item_id = NEW.id), '')
    FROM media_search_map map
    WHERE map.item_id = NEW.id;
END;

CREATE TRIGGER media_items_search_ad AFTER DELETE ON media_items BEGIN
    DELETE FROM media_search
    WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = OLD.id);
    DELETE FROM media_search_map WHERE item_id = OLD.id;
END;

CREATE TRIGGER item_aliases_search_ai AFTER INSERT ON item_aliases BEGIN
    UPDATE media_search
    SET aliases = COALESCE((SELECT group_concat(alias, ' ')
                            FROM item_aliases
                            WHERE item_id = NEW.item_id), '')
    WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = NEW.item_id);
END;

CREATE TRIGGER item_aliases_search_au AFTER UPDATE OF alias, alias_normalized, item_id ON item_aliases
WHEN OLD.alias IS NOT NEW.alias
  OR OLD.alias_normalized IS NOT NEW.alias_normalized
  OR OLD.item_id IS NOT NEW.item_id
BEGIN
    UPDATE media_search
    SET aliases = COALESCE((SELECT group_concat(alias, ' ')
                            FROM item_aliases
                            WHERE item_id = NEW.item_id), '')
    WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = NEW.item_id);
    UPDATE media_search
    SET aliases = COALESCE((SELECT group_concat(alias, ' ')
                            FROM item_aliases
                            WHERE item_id = OLD.item_id), '')
    WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = OLD.item_id)
      AND OLD.item_id IS NOT NEW.item_id;
END;

CREATE TRIGGER item_aliases_search_ad AFTER DELETE ON item_aliases BEGIN
    UPDATE media_search
    SET aliases = COALESCE((SELECT group_concat(alias, ' ')
                            FROM item_aliases
                            WHERE item_id = OLD.item_id), '')
    WHERE rowid = (SELECT fts_rowid FROM media_search_map WHERE item_id = OLD.item_id);
END;
