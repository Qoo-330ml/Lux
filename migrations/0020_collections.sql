CREATE TABLE collections (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL UNIQUE REFERENCES media_items(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    title TEXT NOT NULL,
    overview TEXT,
    poster_path TEXT,
    backdrop_path TEXT,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (library_id, provider, provider_id)
);

CREATE TABLE collection_items (
    collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    sort_order INTEGER NOT NULL,
    PRIMARY KEY (collection_id, item_id)
);

CREATE INDEX idx_collection_items_item ON collection_items(item_id, collection_id);
