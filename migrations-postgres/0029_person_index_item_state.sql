CREATE TABLE person_index_item_state (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    source_fingerprint TEXT,
    relation_schema_version BIGINT NOT NULL DEFAULT 2,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);
