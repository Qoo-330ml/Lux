-- PostgreSQL half of 0249_media_item_facets_tag.sql: the facet kind constraint learns TAG so the
-- content-restriction index worker can project item-level NFO `<tag>` values, and every visible
-- item is queued again for the bounded background backfill.

ALTER TABLE media_item_facets
    DROP CONSTRAINT IF EXISTS media_item_facets_kind_check;

ALTER TABLE media_item_facets
    ADD CONSTRAINT media_item_facets_kind_check
        CHECK (kind IN ('GENRE', 'STUDIO', 'TAG'));

INSERT INTO media_content_restriction_index_queue (item_id)
SELECT item.id
FROM media_items item
WHERE item.removed_at IS NULL
ON CONFLICT (item_id) DO UPDATE SET queued_at = unixepoch();

-- The index worker reads the derived snapshot in `media_item_nfo_cache`, which was written before
-- the parser knew about `<tag>`: re-queueing alone would only rebuild GENRE/STUDIO. Clear the item
-- fingerprint so the local metadata pipeline rebuilds that snapshot from the NFO sidecar (a missing
-- fingerprint is the `rich_cache_missing` signal that makes `enrich_nfo_item_with_metadata` re-read
-- the file). The cache row itself stays readable until the rebuild replaces it.
UPDATE media_items
SET nfo_metadata_fingerprint = NULL
WHERE nfo_metadata_fingerprint IS NOT NULL;

-- The local metadata worker only revisits files whose sidecar changed, so re-arm the bounded
-- backfill for every root: the next scan walks each entry again and the rewritten snapshot queues
-- the item for a TAG re-index.
UPDATE scan_local_metadata_backfills
SET status = 'PENDING',
    cursor_entry_id = NULL,
    next_attempt_at = NULL,
    error = NULL,
    updated_at = unixepoch();
