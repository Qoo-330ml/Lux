-- media_items INSERT already enforces the item id primary key before its
-- AFTER trigger runs, so media_search cannot conflict on item_id here.

CREATE OR REPLACE FUNCTION lux_refresh_media_search_items_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
    SELECT n.id,
           n.title,
           n.sort_title,
           COALESCE(n.original_title, ''),
           ''
    FROM new_rows n;
    RETURN NULL;
END;
$$;
