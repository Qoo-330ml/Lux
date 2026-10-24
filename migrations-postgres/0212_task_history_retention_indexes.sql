-- Support bounded daily retention sweeps for finished background jobs.
CREATE INDEX idx_scan_jobs_retention
    ON scan_jobs(status, created_at, id)
    WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED');

CREATE INDEX idx_metadata_reidentify_jobs_retention
    ON metadata_reidentify_jobs(status, created_at, id)
    WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED');

CREATE INDEX idx_chapter_detection_jobs_retention
    ON chapter_detection_jobs(status, created_at, id)
    WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED');

CREATE INDEX idx_danmaku_match_jobs_retention
    ON danmaku_match_jobs(status, created_at, id)
    WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED');

CREATE INDEX idx_strm_probe_jobs_retention
    ON strm_probe_jobs(status, created_at, id)
    WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED');
