ALTER TABLE libraries
ADD COLUMN realtime_metadata_auto_match_enabled INTEGER NOT NULL DEFAULT 0
CHECK (realtime_metadata_auto_match_enabled IN (0, 1));

ALTER TABLE scan_jobs
ADD COLUMN auto_metadata_match INTEGER NOT NULL DEFAULT 0
CHECK (auto_metadata_match IN (0, 1));
