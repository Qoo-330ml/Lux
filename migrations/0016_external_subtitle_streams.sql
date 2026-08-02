ALTER TABLE media_streams ADD COLUMN external_path TEXT;
ALTER TABLE media_streams ADD COLUMN is_external INTEGER NOT NULL DEFAULT 0 CHECK (is_external IN (0, 1));
ALTER TABLE media_streams ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1));
ALTER TABLE media_streams ADD COLUMN is_forced INTEGER NOT NULL DEFAULT 0 CHECK (is_forced IN (0, 1));

CREATE INDEX idx_media_streams_external_path
ON media_streams(media_source_id, external_path)
WHERE external_path IS NOT NULL;
