UPDATE media_sources
SET probe_status = 'NOT_CONFIGURED', probe_error = NULL,
    updated_at = extract(epoch FROM now())::BIGINT
WHERE source_kind = 'STRM_URL' AND probe_status = 'PENDING';
