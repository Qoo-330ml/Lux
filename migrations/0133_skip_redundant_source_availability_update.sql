DROP TRIGGER IF EXISTS trg_media_sources_availability_insert;

CREATE TRIGGER trg_media_sources_availability_insert
AFTER INSERT ON media_sources
WHEN COALESCE((
    SELECT has_available_source FROM media_items WHERE id = NEW.item_id
), 0) = 0
BEGIN
    UPDATE media_items
    SET has_available_source = 1
    WHERE id = NEW.item_id
      AND has_available_source = 0
      AND EXISTS (
          SELECT 1
          FROM filesystem_entries
          WHERE id = NEW.filesystem_entry_id
            AND is_missing = 0
      );
END;
