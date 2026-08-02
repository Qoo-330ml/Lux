ALTER TABLE media_sources ADD COLUMN probe_error TEXT;

CREATE TABLE media_streams (
    id TEXT PRIMARY KEY NOT NULL,
    media_source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    stream_index INTEGER NOT NULL,
    stream_type TEXT NOT NULL CHECK (stream_type IN ('VIDEO', 'AUDIO', 'SUBTITLE')),
    codec TEXT,
    language TEXT,
    title TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (media_source_id, stream_index)
);

CREATE INDEX idx_media_streams_source_id ON media_streams(media_source_id);
