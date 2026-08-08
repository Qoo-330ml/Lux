ALTER TABLE media_items
ADD COLUMN has_available_source INTEGER NOT NULL DEFAULT 0
CHECK (has_available_source IN (0, 1));

UPDATE media_items
SET has_available_source = EXISTS (
    SELECT 1
    FROM media_sources ms
    JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
    WHERE ms.item_id = media_items.id
      AND fe.is_missing = 0
);

CREATE TRIGGER trg_media_sources_availability_insert
AFTER INSERT ON media_sources
BEGIN
    UPDATE media_items
    SET has_available_source = 1
    WHERE id = NEW.item_id
      AND EXISTS (
          SELECT 1
          FROM filesystem_entries
          WHERE id = NEW.filesystem_entry_id
            AND is_missing = 0
      );
END;

CREATE TRIGGER trg_media_sources_availability_update
AFTER UPDATE OF item_id, filesystem_entry_id ON media_sources
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id IN (OLD.item_id, NEW.item_id);
END;

CREATE TRIGGER trg_media_sources_availability_delete
AFTER DELETE ON media_sources
BEGIN
    UPDATE media_items
    SET has_available_source = EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id
          AND fe.is_missing = 0
    )
    WHERE id = OLD.item_id;
END;

CREATE TRIGGER trg_filesystem_entries_availability_update
AFTER UPDATE OF is_missing ON filesystem_entries
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
