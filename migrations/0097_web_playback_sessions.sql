CREATE TABLE web_playback_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    media_source_id TEXT REFERENCES media_sources(id) ON DELETE SET NULL,
    play_session_id TEXT NOT NULL,
    tier INTEGER NOT NULL CHECK (tier >= 0 AND tier <= 4),
    plan TEXT NOT NULL CHECK (plan IN ('DIRECT', 'SERVER_HLS')),
    state TEXT NOT NULL CHECK (state IN ('ACTIVE', 'STOPPED', 'FAILED')),
    temp_dir TEXT,
    expires_at INTEGER NOT NULL,
    last_heartbeat_at INTEGER NOT NULL,
    last_sequence INTEGER NOT NULL DEFAULT -1,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (user_id, play_session_id)
);

CREATE INDEX idx_web_playback_sessions_expiry
ON web_playback_sessions(state, expires_at);

CREATE TABLE web_playback_events (
    session_id TEXT NOT NULL REFERENCES web_playback_sessions(id) ON DELETE CASCADE,
    event_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    state TEXT NOT NULL CHECK (state IN ('PLAYING', 'PAUSED', 'STOPPED')),
    position_ticks INTEGER NOT NULL CHECK (position_ticks >= 0),
    duration_ticks INTEGER CHECK (duration_ticks IS NULL OR duration_ticks >= 0),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (session_id, event_id),
    UNIQUE (session_id, sequence)
);
