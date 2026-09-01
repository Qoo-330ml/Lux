ALTER TABLE danmaku_match_jobs
    DROP CONSTRAINT IF EXISTS danmaku_match_jobs_concurrency_check;

ALTER TABLE danmaku_match_jobs
    ADD CONSTRAINT danmaku_match_jobs_concurrency_check
    CHECK (concurrency BETWEEN 0 AND 64);
