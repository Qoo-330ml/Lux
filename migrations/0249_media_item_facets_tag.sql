-- LUX-900: `media_item_facets` learns the `TAG` kind so the content-restriction index worker can
-- project the item-level NFO `<tag>` values next to the existing GENRE/STUDIO rows. SQLite cannot
-- alter a CHECK constraint in place, so the table is rebuilt with the same primary key, the same
-- cascade to `media_items` and the same lookup index; the rows already written are copied over.

CREATE TABLE media_item_facets_new (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('GENRE', 'STUDIO', 'TAG')),
    name_key TEXT NOT NULL CHECK (length(trim(name_key)) BETWEEN 1 AND 256),
    name TEXT NOT NULL CHECK (length(trim(name)) BETWEEN 1 AND 256),
    PRIMARY KEY (item_id, kind, name_key)
);

INSERT INTO media_item_facets_new (item_id, kind, name_key, name)
SELECT item_id, kind, name_key, name
FROM media_item_facets;

DROP TABLE media_item_facets;

ALTER TABLE media_item_facets_new RENAME TO media_item_facets;

CREATE INDEX idx_media_item_facets_lookup
    ON media_item_facets(kind, name_key, item_id);

-- Existing items have no TAG facet rows yet: queue every visible item so the worker backfills in
-- bounded batches (it already owns the durable queue and the stale-snapshot check).
INSERT INTO media_content_restriction_index_queue (item_id)
SELECT item.id
FROM media_items item
WHERE item.removed_at IS NULL
ON CONFLICT(item_id) DO UPDATE SET queued_at = unixepoch();

-- The index worker reads the derived snapshot in `media_item_nfo_cache`, which was written before
-- the parser knew about `<tag>`: re-queueing alone would only rebuild GENRE/STUDIO. Clear the item
-- fingerprint so the local metadata pipeline rebuilds that snapshot from the NFO sidecar:
-- `LocalNfoMetadataStore::read_item_if_usable_with_fingerprint` returns `None` without a valid
-- fingerprint, and `MetadataEnricher::enrich_nfo_item_with_metadata` treats that `rich_cache_missing`
-- state as "re-read the file". The cache row itself stays readable, so list/detail fields keep
-- working until the rebuild replaces them.
UPDATE media_items
SET nfo_metadata_fingerprint = NULL
WHERE nfo_metadata_fingerprint IS NOT NULL;

-- The local metadata worker only revisits files whose sidecar changed, so re-arm the bounded
-- backfill for every root: the next scan walks each entry again, re-parses the sidecars whose
-- snapshot was invalidated above, and the rewritten snapshot queues the item for a TAG re-index.
UPDATE scan_local_metadata_backfills
SET status = 'PENDING',
    cursor_entry_id = NULL,
    next_attempt_at = NULL,
    error = NULL,
    updated_at = unixepoch();
