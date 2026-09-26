-- A newly inserted media item cannot have aliases before its row exists.
-- Alias changes already refresh media_search through the item_aliases trigger,
-- so media item refreshes must not aggregate item_aliases once per row.

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
    FROM new_rows n
    ON CONFLICT (item_id) DO UPDATE SET
        title = EXCLUDED.title,
        sort_title = EXCLUDED.sort_title,
        original_title = EXCLUDED.original_title,
        aliases = EXCLUDED.aliases;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION lux_refresh_media_search_items_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
    SELECT n.id,
           n.title,
           n.sort_title,
           COALESCE(n.original_title, ''),
           COALESCE(existing.aliases, '')
    FROM new_rows n
    LEFT JOIN media_search existing ON existing.item_id = n.id
    ON CONFLICT (item_id) DO UPDATE SET
        title = EXCLUDED.title,
        sort_title = EXCLUDED.sort_title,
        original_title = EXCLUDED.original_title,
        aliases = EXCLUDED.aliases;
    RETURN NULL;
END;
$$;
