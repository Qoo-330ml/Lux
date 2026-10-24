-- Merge provenance so an administrator merge (manual or automatic) can be undone.
-- One record describes one merged root item; the two state tables capture what the merge changed:
-- item hierarchy fields and media source ownership/default flags.
CREATE TABLE media_merge_records (
    id TEXT PRIMARY KEY,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    item_type TEXT NOT NULL,
    primary_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    merged_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    merged_at BIGINT NOT NULL,
    undone_at BIGINT
);

CREATE INDEX idx_media_merge_records_primary
    ON media_merge_records(primary_item_id, undone_at);

CREATE INDEX idx_media_merge_records_merged
    ON media_merge_records(merged_item_id);

CREATE TABLE media_merge_item_states (
    record_id TEXT NOT NULL REFERENCES media_merge_records(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    item_type TEXT NOT NULL,
    parent_id TEXT,
    series_id TEXT,
    merged_into_item_id TEXT,
    PRIMARY KEY (record_id, item_id)
);

CREATE TABLE media_merge_source_states (
    record_id TEXT NOT NULL REFERENCES media_merge_records(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    is_default INTEGER NOT NULL,
    PRIMARY KEY (record_id, source_id)
);
