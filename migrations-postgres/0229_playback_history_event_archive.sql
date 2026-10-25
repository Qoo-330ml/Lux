-- Playback history events that outlive the media item they were recorded against. See the SQLite
-- migration 0227_playback_history_event_archive.sql for the reasoning; the schema is the same, with
-- BIGINT integers as everywhere else in this directory.
CREATE TABLE playback_history_event_archive (
    id TEXT NOT NULL,
    item_type TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (
        event_type IN ('PLAY_STARTED', 'PLAY_PROGRESS', 'PAUSED', 'STOPPED', 'MARKED_PLAYED')
    ),
    position_ticks BIGINT NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    duration_ticks BIGINT CHECK (duration_ticks IS NULL OR duration_ticks >= 0),
    occurred_at BIGINT NOT NULL,
    source TEXT NOT NULL,
    source_event_key TEXT NOT NULL,
    detached_at BIGINT NOT NULL DEFAULT (unixepoch()),
    source_item_id TEXT NOT NULL,
    PRIMARY KEY (id, provider, provider_id)
);

CREATE INDEX idx_playback_history_event_archive_identity
    ON playback_history_event_archive(item_type, provider, provider_id);
