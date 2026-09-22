ALTER TABLE media_items
ADD COLUMN updated_at INTEGER NOT NULL DEFAULT (unixepoch());

CREATE INDEX idx_media_items_updated_at
ON media_items(updated_at, id);
