ALTER TABLE media_items
ADD COLUMN updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT);

CREATE INDEX idx_media_items_updated_at
ON media_items(updated_at, id);
