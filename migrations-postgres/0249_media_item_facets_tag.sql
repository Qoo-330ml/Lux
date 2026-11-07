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
