CREATE TABLE person_index_item_state (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    source_fingerprint TEXT,
    relation_schema_version INTEGER NOT NULL DEFAULT 2,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
