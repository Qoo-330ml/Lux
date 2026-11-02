-- Per-item genre and studio projection of the cached NFO details. The Emby /Genres, /Studios
-- endpoints and the /Items Genres/Studios filters read this table so a user request never parses
-- NFO JSON or scans a whole library. The existing content-restriction index worker fills it from
-- the same NFO snapshot (it already owns the durable queue and the stale-snapshot check).
-- Only values the NFO parser keeps are stored; `name` is the display text of the first spelling
-- seen for the item, `name_key` is the lower-cased lookup key.

CREATE TABLE media_item_facets (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('GENRE', 'STUDIO')),
    name_key TEXT NOT NULL CHECK (length(trim(name_key)) BETWEEN 1 AND 256),
    name TEXT NOT NULL CHECK (length(trim(name)) BETWEEN 1 AND 256),
    PRIMARY KEY (item_id, kind, name_key)
);

CREATE INDEX idx_media_item_facets_lookup
    ON media_item_facets(kind, name_key, item_id);

-- Existing items have no facet rows yet: queue them so the worker backfills in bounded batches.
INSERT INTO media_content_restriction_index_queue (item_id)
SELECT item.id
FROM media_items item
WHERE item.removed_at IS NULL
ON CONFLICT(item_id) DO UPDATE SET queued_at = unixepoch();
