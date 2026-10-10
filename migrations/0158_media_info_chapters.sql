CREATE TABLE media_info_chapters (
    id TEXT PRIMARY KEY NOT NULL,
    media_source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    start_position_ticks INTEGER NOT NULL CHECK (start_position_ticks >= 0),
    name TEXT,
    chapter_index INTEGER NOT NULL CHECK (chapter_index >= 0),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (media_source_id, chapter_index)
);

CREATE INDEX idx_media_info_chapters_source_position
    ON media_info_chapters(media_source_id, start_position_ticks, chapter_index, id);
