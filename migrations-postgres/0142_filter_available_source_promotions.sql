-- A source INSERT only promotes unavailable items. Filter already available
-- items before probing filesystem_entries so large scan batches do not repeat
-- the availability lookup for rows that are already known to be available.

CREATE OR REPLACE FUNCTION lux_refresh_media_sources_availability_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE media_items item
    SET has_available_source = 1
    FROM (
        SELECT DISTINCT incoming.item_id
        FROM new_rows incoming
        JOIN media_items candidate
          ON candidate.id = incoming.item_id
         AND candidate.has_available_source = 0
        JOIN filesystem_entries entry
          ON entry.id = incoming.filesystem_entry_id
         AND entry.is_missing = 0
    ) affected
    WHERE item.id = affected.item_id
      AND item.has_available_source = 0;
    RETURN NULL;
END;
$$;
