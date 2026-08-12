CREATE TABLE media_chapters (
    id TEXT PRIMARY KEY NOT NULL,
    media_source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    start_position_ticks INTEGER NOT NULL CHECK (start_position_ticks >= 0),
    name TEXT,
    marker_type TEXT NOT NULL CHECK (marker_type IN ('INTRO_START', 'INTRO_END', 'CREDITS_START')),
    chapter_index INTEGER NOT NULL CHECK (chapter_index >= 0),
    provider_id TEXT NOT NULL CHECK (length(trim(provider_id)) BETWEEN 1 AND 256),
    confidence REAL NOT NULL CHECK (confidence >= 0.0 AND confidence <= 1.0),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (media_source_id, provider_id, marker_type)
);

CREATE INDEX idx_media_chapters_source_position
    ON media_chapters(media_source_id, start_position_ticks, chapter_index, id);
