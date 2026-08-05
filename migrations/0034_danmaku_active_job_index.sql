CREATE UNIQUE INDEX idx_danmaku_match_jobs_one_active_library
    ON danmaku_match_jobs(library_id)
    WHERE status IN ('PENDING', 'RUNNING');
