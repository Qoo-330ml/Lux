UPDATE media_sources
SET probe_status = 'PENDING', probe_error = NULL
WHERE source_kind = 'STRM_URL';
