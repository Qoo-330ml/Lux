-- Playback-triggered STRM probes must reuse the exact User-Agent that requested the direct link,
-- because some providers bind a short-lived media link to that UA and reject a second UA for the
-- same file. Bulk jobs keep NULL and let the plugin use its default UA.
ALTER TABLE strm_probe_jobs ADD COLUMN target_user_agent TEXT;
