CREATE OR REPLACE FUNCTION lux_refresh_item_availability()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    source_item_id TEXT;
BEGIN
    IF TG_TABLE_NAME = 'media_sources' THEN
        IF TG_OP = 'INSERT' THEN
            UPDATE media_items
            SET has_available_source = 1
            WHERE id = NEW.item_id
              AND has_available_source = 0
              AND EXISTS (
                  SELECT 1
                  FROM filesystem_entries fe
                  WHERE fe.id = NEW.filesystem_entry_id
                    AND fe.is_missing = 0
              );
            RETURN NEW;
        END IF;

        source_item_id := CASE WHEN TG_OP = 'DELETE' THEN OLD.item_id ELSE NEW.item_id END;
        UPDATE media_items
        SET has_available_source = CASE WHEN EXISTS (
            SELECT 1
            FROM media_sources ms
            JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
            WHERE ms.item_id = media_items.id AND fe.is_missing = 0
        ) THEN 1 ELSE 0 END
        WHERE id = source_item_id OR (TG_OP = 'UPDATE' AND id = OLD.item_id);
        RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
    END IF;

    UPDATE media_items
    SET has_available_source = CASE WHEN EXISTS (
        SELECT 1
        FROM media_sources ms
        JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
        WHERE ms.item_id = media_items.id AND fe.is_missing = 0
    ) THEN 1 ELSE 0 END
    WHERE id IN (
        SELECT item_id FROM media_sources WHERE filesystem_entry_id = NEW.id
    );
    RETURN NEW;
END;
$$;
