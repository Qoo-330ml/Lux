-- Playback history events that outlive the media item they were recorded against.
--
-- `playback_history_events` cascades from `media_items`, so deleting a library (or purging an
-- expired tombstone) erased the imported history of everything in it. The events are a user's
-- record of what they actually watched, and re-adding the same title elsewhere should bring that
-- record back just like `user_item_state_archive` does for played/favourite/resume state.
--
-- One archive row per (event, provider identity), the same identity `media_item_provider_ids`
-- indexes. An event usually has one identity, but keeping the identity in the key means an event is
-- still found when only one of its identities is re-confirmed. Archiving is idempotent, so a
-- resumed deletion job folds the same rows in again.
--
-- `source` + `source_event_key` stay the importer's idempotency key: reattaching an event uses
-- `ON CONFLICT DO NOTHING`, so a history row that survived elsewhere is never duplicated.
CREATE TABLE playback_history_event_archive (
    id TEXT NOT NULL,
    item_type TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL CHECK (
        event_type IN ('PLAY_STARTED', 'PLAY_PROGRESS', 'PAUSED', 'STOPPED', 'MARKED_PLAYED')
    ),
    position_ticks INTEGER NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    duration_ticks INTEGER CHECK (duration_ticks IS NULL OR duration_ticks >= 0),
    occurred_at INTEGER NOT NULL,
    source TEXT NOT NULL,
    source_event_key TEXT NOT NULL,
    detached_at INTEGER NOT NULL DEFAULT (unixepoch()),
    -- The item the event pointed at. Kept for operator support, never read by the reconciler.
    source_item_id TEXT NOT NULL,
    PRIMARY KEY (id, provider, provider_id)
);

-- The reconciler looks rows up by identity, and batches them by event so one event is consumed
-- exactly once even when several of its identities match.
CREATE INDEX idx_playback_history_event_archive_identity
    ON playback_history_event_archive(item_type, provider, provider_id);
