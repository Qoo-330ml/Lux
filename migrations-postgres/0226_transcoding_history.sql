-- Completed, failed and expired server-side transcodes, for the admin transcoding page.
CREATE TABLE transcoding_history (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    media_source_id TEXT,
    tier BIGINT NOT NULL,
    outcome TEXT NOT NULL,
    started_at BIGINT NOT NULL,
    finished_at BIGINT NOT NULL,
    wall_seconds BIGINT NOT NULL,
    encoded_seconds BIGINT,
    average_speed DOUBLE PRECISION,
    last_fps DOUBLE PRECISION,
    output_bytes BIGINT,
    failure_reason TEXT,
    created_at BIGINT NOT NULL
);

CREATE INDEX idx_transcoding_history_finished
    ON transcoding_history(finished_at DESC, id DESC);

CREATE INDEX idx_transcoding_history_user
    ON transcoding_history(user_id, finished_at DESC);
