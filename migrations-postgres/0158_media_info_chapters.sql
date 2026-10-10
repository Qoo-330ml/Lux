CREATE TABLE media_info_chapters (
    id TEXT PRIMARY KEY NOT NULL,
    media_source_id TEXT NOT NULL REFERENCES media_sources(id) ON DELETE CASCADE,
    start_position_ticks BIGINT NOT NULL CHECK (start_position_ticks >= 0),
    name TEXT,
    chapter_index BIGINT NOT NULL CHECK (chapter_index >= 0),
    created_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    updated_at BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT,
    UNIQUE (media_source_id, chapter_index)
);

CREATE INDEX idx_media_info_chapters_source_position
    ON media_info_chapters(media_source_id, start_position_ticks, chapter_index, id);
