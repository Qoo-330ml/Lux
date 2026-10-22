-- Keep a durable queue of items whose content-rating index is missing or stale.
-- The previous worker recovered work by scanning every visible media item every
-- ten seconds; with the queue it only reads the rows that changed.

CREATE TABLE media_content_restriction_index_queue (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    queued_at INTEGER NOT NULL DEFAULT (unixepoch())
);

INSERT INTO media_content_restriction_index_queue (item_id)
SELECT item.id
FROM media_items item
LEFT JOIN media_content_restriction_index restriction_index
    ON restriction_index.item_id = item.id
WHERE item.removed_at IS NULL
  AND restriction_index.item_id IS NULL;

DROP TRIGGER IF EXISTS trg_media_items_content_restriction_index_invalidate_nfo;

CREATE TRIGGER trg_media_items_content_restriction_index_invalidate_nfo
AFTER UPDATE OF nfo_metadata_json ON media_items
WHEN OLD.nfo_metadata_json IS NOT NEW.nfo_metadata_json
BEGIN
    DELETE FROM media_content_restriction_index WHERE item_id = NEW.id;
    INSERT INTO media_content_restriction_index_queue (item_id)
    VALUES (NEW.id)
    ON CONFLICT(item_id) DO UPDATE SET queued_at = unixepoch();
END;

CREATE TRIGGER trg_media_items_content_restriction_index_queue_insert
AFTER INSERT ON media_items
BEGIN
    INSERT INTO media_content_restriction_index_queue (item_id)
    VALUES (NEW.id)
    ON CONFLICT(item_id) DO UPDATE SET queued_at = unixepoch();
END;
