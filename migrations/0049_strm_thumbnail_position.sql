ALTER TABLE strm_probe_jobs
ADD COLUMN thumbnail_position_percent INTEGER NOT NULL DEFAULT 30
CHECK (thumbnail_position_percent BETWEEN 1 AND 99);
