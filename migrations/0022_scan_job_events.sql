CREATE TABLE scan_job_events (
    id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL REFERENCES scan_jobs(id) ON DELETE CASCADE,
    level TEXT NOT NULL CHECK (level IN ('INFO', 'WARN', 'ERROR')),
    event_code TEXT NOT NULL,
    message TEXT NOT NULL,
    details_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_scan_job_events_job_created
    ON scan_job_events(job_id, created_at DESC, id DESC);
CREATE INDEX idx_scan_job_events_code
    ON scan_job_events(event_code, created_at DESC, id DESC);
