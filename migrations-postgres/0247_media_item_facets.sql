-- PostgreSQL half of 0245_media_item_facets.sql: per-item genre/studio projection that backs the
-- Emby /Genres, /Studios endpoints and the /Items Genres/Studios filters, filled by the
-- content-restriction index worker from the same NFO snapshot.

CREATE TABLE media_item_facets (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('GENRE', 'STUDIO')),
    name_key TEXT NOT NULL CHECK (char_length(btrim(name_key)) BETWEEN 1 AND 256),
    name TEXT NOT NULL CHECK (char_length(btrim(name)) BETWEEN 1 AND 256),
    PRIMARY KEY (item_id, kind, name_key)
);

CREATE INDEX idx_media_item_facets_lookup
    ON media_item_facets(kind, name_key, item_id);

INSERT INTO media_content_restriction_index_queue (item_id)
SELECT item.id
FROM media_items item
WHERE item.removed_at IS NULL
ON CONFLICT (item_id) DO UPDATE SET queued_at = unixepoch();
