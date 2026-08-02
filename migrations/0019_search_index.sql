CREATE TABLE item_aliases (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    alias TEXT NOT NULL,
    language TEXT,
    alias_normalized TEXT NOT NULL,
    UNIQUE (item_id, alias_normalized)
);

CREATE INDEX idx_item_aliases_item_id ON item_aliases(item_id);

CREATE VIRTUAL TABLE media_search USING fts5(
    item_id UNINDEXED,
    title,
    sort_title,
    original_title,
    aliases
);

INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
SELECT mi.id, mi.title, mi.sort_title, COALESCE(mi.original_title, ''),
       COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = mi.id), '')
FROM media_items mi;

CREATE TRIGGER media_items_search_ai AFTER INSERT ON media_items BEGIN
    INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
    VALUES (NEW.id, NEW.title, NEW.sort_title, COALESCE(NEW.original_title, ''),
            COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.id), ''));
END;

CREATE TRIGGER media_items_search_au AFTER UPDATE OF title, sort_title, original_title ON media_items BEGIN
    DELETE FROM media_search WHERE item_id = OLD.id;
    INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
    VALUES (NEW.id, NEW.title, NEW.sort_title, COALESCE(NEW.original_title, ''),
            COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.id), ''));
END;

CREATE TRIGGER media_items_search_ad AFTER DELETE ON media_items BEGIN
    DELETE FROM media_search WHERE item_id = OLD.id;
END;

CREATE TRIGGER item_aliases_search_ai AFTER INSERT ON item_aliases BEGIN
    UPDATE media_search
    SET aliases = COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.item_id), '')
    WHERE item_id = NEW.item_id;
END;

CREATE TRIGGER item_aliases_search_au AFTER UPDATE OF alias, alias_normalized, item_id ON item_aliases BEGIN
    UPDATE media_search
    SET aliases = COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.item_id), '')
    WHERE item_id = NEW.item_id;
END;

CREATE TRIGGER item_aliases_search_ad AFTER DELETE ON item_aliases BEGIN
    UPDATE media_search
    SET aliases = COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = OLD.item_id), '')
    WHERE item_id = OLD.item_id;
END;
