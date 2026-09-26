-- A source INSERT can only promote availability. Recompute availability only
-- for source rows that point at a currently present filesystem entry; deletes,
-- source moves, and filesystem disappearance keep the full reconciliation path.

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
        JOIN filesystem_entries entry
          ON entry.id = incoming.filesystem_entry_id
         AND entry.is_missing = 0
    ) affected
    WHERE item.id = affected.item_id
      AND item.has_available_source = 0;
    RETURN NULL;
END;
$$;
