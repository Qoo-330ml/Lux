ALTER TABLE libraries
    ADD COLUMN system_kind TEXT
        CHECK (system_kind IS NULL OR system_kind = 'COLLECTIONS');

CREATE UNIQUE INDEX idx_libraries_system_kind
    ON libraries(system_kind)
    WHERE system_kind IS NOT NULL;

INSERT INTO libraries (id, name, kind, system_kind)
VALUES ('00000000-0000-7000-8000-000000000538', '合集', 'MIXED', 'COLLECTIONS');

CREATE TABLE collection_item_references (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    movie_provider TEXT NOT NULL,
    movie_provider_id TEXT NOT NULL,
    collection_provider TEXT NOT NULL,
    collection_provider_id TEXT NOT NULL,
    collection_title TEXT NOT NULL,
    sort_order BIGINT,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (item_id, collection_provider, collection_provider_id)
);

CREATE INDEX idx_collection_item_references_identity
    ON collection_item_references(
        collection_provider, collection_provider_id,
        movie_provider, movie_provider_id
    );

CREATE TABLE collection_work_items (
    work_item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    movie_provider TEXT NOT NULL,
    movie_provider_id TEXT NOT NULL,
    UNIQUE (movie_provider, movie_provider_id)
);

CREATE TABLE collection_work_sources (
    work_item_id TEXT NOT NULL REFERENCES collection_work_items(work_item_id) ON DELETE CASCADE,
    source_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    sort_order BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (work_item_id, source_item_id),
    UNIQUE (source_item_id)
);

CREATE INDEX idx_collection_work_sources_source
    ON collection_work_sources(source_item_id, work_item_id);
