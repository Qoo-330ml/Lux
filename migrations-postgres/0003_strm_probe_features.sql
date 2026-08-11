ALTER TABLE strm_probe_jobs
ADD COLUMN media_info_enabled BIGINT NOT NULL DEFAULT 1
CHECK (media_info_enabled IN (0, 1));

ALTER TABLE strm_probe_jobs
ADD COLUMN thumbnail_enabled BIGINT NOT NULL DEFAULT 0
CHECK (thumbnail_enabled IN (0, 1));
