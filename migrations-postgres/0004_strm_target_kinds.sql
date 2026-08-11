ALTER TABLE media_sources
ADD COLUMN strm_target_kind TEXT
CHECK (strm_target_kind IN ('URL', 'PATH', 'OPAQUE', 'EMPTY'));
