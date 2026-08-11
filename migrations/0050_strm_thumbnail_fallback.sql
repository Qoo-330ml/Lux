ALTER TABLE media_items
ADD COLUMN thumbnail_fallback_required INTEGER NOT NULL DEFAULT 0
CHECK (thumbnail_fallback_required IN (0, 1));
