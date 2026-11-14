-- Playback-triggered probes are single-source jobs. They must run even while a bulk job covers the
-- same library, so the active-job uniqueness keeps one bulk job per library and adds at most one
-- active job per source on top of it.
DROP INDEX idx_strm_probe_jobs_one_active_library;
CREATE UNIQUE INDEX idx_strm_probe_jobs_one_active_library
    ON strm_probe_jobs(library_id, coalesce(target_source_id, ''))
    WHERE status IN ('PENDING', 'RUNNING');
