ALTER TABLE media_items
ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;

UPDATE media_items
SET updated_at = unixepoch()
WHERE updated_at = 0;

CREATE TRIGGER media_items_updated_at_ai
AFTER INSERT ON media_items
WHEN NEW.updated_at = 0
BEGIN
    UPDATE media_items
    SET updated_at = unixepoch()
    WHERE id = NEW.id;
END;

CREATE INDEX idx_media_items_updated_at
ON media_items(updated_at, id);
