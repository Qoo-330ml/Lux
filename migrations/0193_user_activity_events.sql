CREATE TABLE user_activity_events (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL,
    actor_user_id TEXT,
    event_type TEXT NOT NULL CHECK (length(trim(event_type)) BETWEEN 1 AND 128),
    target_type TEXT,
    target_id TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    occurred_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_user_activity_events_user_time
    ON user_activity_events(user_id, occurred_at DESC, id DESC);

CREATE INDEX idx_user_activity_events_user_type_time
    ON user_activity_events(user_id, event_type, occurred_at DESC, id DESC);

CREATE INDEX idx_user_activity_events_retention
    ON user_activity_events(occurred_at, id);
