-- Move the parsed NFO projection out of the hot media_items row.
--
-- The projection can reach 1 MiB per item and is only needed by detail endpoints, the content
-- restriction index worker and collection-reference reconciliation. Keeping it in a side table
-- lets media_items updates stay small while the cache row is written only when its content changes.
CREATE TABLE media_item_nfo_cache (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    fingerprint BYTEA,
    json TEXT NOT NULL,
    bytes BIGINT NOT NULL CHECK (bytes >= 0),
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM now())::BIGINT)
);

INSERT INTO media_item_nfo_cache (item_id, fingerprint, json, bytes)
SELECT id, nfo_metadata_fingerprint, nfo_metadata_json, octet_length(nfo_metadata_json)
FROM media_items
WHERE nfo_metadata_json IS NOT NULL;
