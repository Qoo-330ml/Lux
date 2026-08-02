CREATE TABLE audit_events (
    id TEXT PRIMARY KEY NOT NULL,
    actor_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    event_type TEXT NOT NULL,
    target_type TEXT,
    target_id TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_audit_events_created_at ON audit_events(created_at DESC, id DESC);
CREATE INDEX idx_audit_events_actor ON audit_events(actor_user_id, created_at DESC);
