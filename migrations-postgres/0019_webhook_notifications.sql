CREATE TABLE notification_destinations (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    enabled BIGINT NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    allow_private_network BIGINT NOT NULL DEFAULT 0 CHECK (allow_private_network IN (0, 1)),
    event_types_json TEXT NOT NULL DEFAULT '[]',
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_notification_destinations_enabled
    ON notification_destinations(enabled, id);

CREATE TABLE notification_events (
    id TEXT PRIMARY KEY NOT NULL,
    event_type TEXT NOT NULL,
    schema_version BIGINT NOT NULL CHECK (schema_version >= 1),
    occurred_at BIGINT NOT NULL,
    dedupe_key TEXT NOT NULL UNIQUE,
    payload_json TEXT NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    expires_at BIGINT
);

CREATE INDEX idx_notification_events_created
    ON notification_events(created_at DESC, id DESC);

CREATE TABLE notification_deliveries (
    id TEXT PRIMARY KEY NOT NULL,
    event_id TEXT NOT NULL REFERENCES notification_events(id) ON DELETE CASCADE,
    destination_id TEXT NOT NULL REFERENCES notification_destinations(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'RUNNING', 'DELIVERED', 'FAILED')),
    attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at BIGINT NOT NULL DEFAULT (unixepoch()),
    claimed_until BIGINT,
    last_http_status BIGINT,
    last_error TEXT,
    delivered_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    UNIQUE (event_id, destination_id)
);

CREATE INDEX idx_notification_deliveries_ready
    ON notification_deliveries(status, next_attempt_at, id);

CREATE INDEX idx_notification_deliveries_destination
    ON notification_deliveries(destination_id, created_at DESC, id DESC);
