ALTER TABLE media_items ADD COLUMN identity_key TEXT;

CREATE UNIQUE INDEX idx_media_items_identity_key
ON media_items(identity_key)
WHERE identity_key IS NOT NULL;
