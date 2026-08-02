CREATE TABLE playback_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    media_source_id TEXT REFERENCES media_sources(id) ON DELETE SET NULL,
    play_session_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    client TEXT,
    device_name TEXT,
    state TEXT NOT NULL CHECK (state IN ('PLAYING', 'PAUSED', 'STOPPED')),
    position_ticks INTEGER NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    duration_ticks INTEGER CHECK (duration_ticks IS NULL OR duration_ticks >= 0),
    is_paused INTEGER NOT NULL DEFAULT 0 CHECK (is_paused IN (0, 1)),
    started_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_event_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (user_id, play_session_id)
);

CREATE INDEX idx_playback_sessions_active
ON playback_sessions(user_id, state, last_event_at);
