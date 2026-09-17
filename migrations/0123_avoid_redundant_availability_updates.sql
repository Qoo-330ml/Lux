DROP TRIGGER IF EXISTS trg_filesystem_entries_availability_update;

CREATE TRIGGER trg_filesystem_entries_availability_update
AFTER UPDATE OF is_missing ON filesystem_entries
WHEN OLD.is_missing <> NEW.is_missing
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id IN (
        SELECT item_id
        FROM media_sources
        WHERE filesystem_entry_id = NEW.id
    );
END;
