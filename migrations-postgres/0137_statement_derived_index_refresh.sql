-- Refresh derived search and availability indexes once per write statement.
-- PostgreSQL transition tables let bulk INSERT/UPDATE/DELETE statements
-- update each affected item as a set instead of invoking a row trigger for
-- every media row, alias, source, or filesystem entry.

DROP TRIGGER IF EXISTS media_items_search_ai ON media_items;
DROP TRIGGER IF EXISTS media_items_search_au ON media_items;
DROP TRIGGER IF EXISTS media_items_search_ad ON media_items;
DROP TRIGGER IF EXISTS item_aliases_search_ai ON item_aliases;
DROP TRIGGER IF EXISTS item_aliases_search_au ON item_aliases;
DROP TRIGGER IF EXISTS item_aliases_search_ad ON item_aliases;
DROP TRIGGER IF EXISTS media_sources_availability_ai ON media_sources;
DROP TRIGGER IF EXISTS media_sources_availability_au ON media_sources;
DROP TRIGGER IF EXISTS media_sources_availability_ad ON media_sources;
DROP TRIGGER IF EXISTS filesystem_entries_availability_au ON filesystem_entries;

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
           COALESCE(a.aliases, '')
    FROM new_rows n
    LEFT JOIN LATERAL (
        SELECT string_agg(ia.alias, ' ') AS aliases
        FROM item_aliases ia
        WHERE ia.item_id = n.id
    ) a ON TRUE
    ON CONFLICT (item_id) DO UPDATE SET
        title = EXCLUDED.title,
        sort_title = EXCLUDED.sort_title,
        original_title = EXCLUDED.original_title,
        aliases = EXCLUDED.aliases;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_items_search_ai
AFTER INSERT ON media_items
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_search_items_insert_stmt();

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
           COALESCE(a.aliases, '')
    FROM new_rows n
    LEFT JOIN LATERAL (
        SELECT string_agg(ia.alias, ' ') AS aliases
        FROM item_aliases ia
        WHERE ia.item_id = n.id
    ) a ON TRUE
    ON CONFLICT (item_id) DO UPDATE SET
        title = EXCLUDED.title,
        sort_title = EXCLUDED.sort_title,
        original_title = EXCLUDED.original_title,
        aliases = EXCLUDED.aliases;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_items_search_au
AFTER UPDATE OF title, sort_title, original_title ON media_items
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_search_items_update_stmt();

CREATE OR REPLACE FUNCTION lux_refresh_media_search_items_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_search s
    USING old_rows o
    WHERE s.item_id = o.id;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_items_search_ad
AFTER DELETE ON media_items
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_search_items_delete_stmt();

CREATE OR REPLACE FUNCTION lux_refresh_media_search_aliases_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE media_search s
    SET aliases = COALESCE(a.aliases, '')
    FROM (
        SELECT DISTINCT item_id
        FROM new_rows
    ) affected
    LEFT JOIN LATERAL (
        SELECT string_agg(ia.alias, ' ') AS aliases
        FROM item_aliases ia
        WHERE ia.item_id = affected.item_id
    ) a ON TRUE
    WHERE s.item_id = affected.item_id;
    RETURN NULL;
END;
$$;

CREATE TRIGGER item_aliases_search_ai
AFTER INSERT ON item_aliases
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_search_aliases_insert_stmt();

CREATE OR REPLACE FUNCTION lux_refresh_media_search_aliases_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE media_search s
    SET aliases = COALESCE(a.aliases, '')
    FROM (
        SELECT item_id FROM old_rows
        UNION
        SELECT item_id FROM new_rows
    ) affected
    LEFT JOIN LATERAL (
        SELECT string_agg(ia.alias, ' ') AS aliases
        FROM item_aliases ia
        WHERE ia.item_id = affected.item_id
    ) a ON TRUE
    WHERE s.item_id = affected.item_id;
    RETURN NULL;
END;
$$;

CREATE TRIGGER item_aliases_search_au
AFTER UPDATE OF alias, alias_normalized, item_id ON item_aliases
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_search_aliases_update_stmt();

CREATE OR REPLACE FUNCTION lux_refresh_media_search_aliases_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE media_search s
    SET aliases = COALESCE(a.aliases, '')
    FROM (
        SELECT DISTINCT item_id
        FROM old_rows
    ) affected
    LEFT JOIN LATERAL (
        SELECT string_agg(ia.alias, ' ') AS aliases
        FROM item_aliases ia
        WHERE ia.item_id = affected.item_id
    ) a ON TRUE
    WHERE s.item_id = affected.item_id;
    RETURN NULL;
END;
$$;

CREATE TRIGGER item_aliases_search_ad
AFTER DELETE ON item_aliases
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_search_aliases_delete_stmt();

CREATE OR REPLACE FUNCTION lux_refresh_media_sources_availability_insert_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    WITH affected AS (
        SELECT DISTINCT item_id
        FROM new_rows
    ), computed AS (
        SELECT a.item_id,
               CASE WHEN EXISTS (
                   SELECT 1
                   FROM media_sources ms
                   JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                   WHERE ms.item_id = a.item_id
                     AND fe.is_missing = 0
               ) THEN 1 ELSE 0 END AS has_available_source
        FROM affected a
    )
    UPDATE media_items m
    SET has_available_source = c.has_available_source
    FROM computed c
    WHERE m.id = c.item_id
      AND m.has_available_source IS DISTINCT FROM c.has_available_source;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_sources_availability_ai
AFTER INSERT ON media_sources
REFERENCING NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_sources_availability_insert_stmt();

CREATE OR REPLACE FUNCTION lux_refresh_media_sources_availability_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    WITH affected AS (
        SELECT item_id FROM old_rows
        UNION
        SELECT item_id FROM new_rows
    ), computed AS (
        SELECT a.item_id,
               CASE WHEN EXISTS (
                   SELECT 1
                   FROM media_sources ms
                   JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                   WHERE ms.item_id = a.item_id
                     AND fe.is_missing = 0
               ) THEN 1 ELSE 0 END AS has_available_source
        FROM affected a
    )
    UPDATE media_items m
    SET has_available_source = c.has_available_source
    FROM computed c
    WHERE m.id = c.item_id
      AND m.has_available_source IS DISTINCT FROM c.has_available_source;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_sources_availability_au
AFTER UPDATE OF item_id, filesystem_entry_id ON media_sources
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_sources_availability_update_stmt();

CREATE OR REPLACE FUNCTION lux_refresh_media_sources_availability_delete_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    WITH affected AS (
        SELECT DISTINCT item_id
        FROM old_rows
    ), computed AS (
        SELECT a.item_id,
               CASE WHEN EXISTS (
                   SELECT 1
                   FROM media_sources ms
                   JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                   WHERE ms.item_id = a.item_id
                     AND fe.is_missing = 0
               ) THEN 1 ELSE 0 END AS has_available_source
        FROM affected a
    )
    UPDATE media_items m
    SET has_available_source = c.has_available_source
    FROM computed c
    WHERE m.id = c.item_id
      AND m.has_available_source IS DISTINCT FROM c.has_available_source;
    RETURN NULL;
END;
$$;

CREATE TRIGGER media_sources_availability_ad
AFTER DELETE ON media_sources
REFERENCING OLD TABLE AS old_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_media_sources_availability_delete_stmt();

CREATE OR REPLACE FUNCTION lux_refresh_filesystem_entries_availability_update_stmt()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    WITH changed_entries AS (
        SELECT n.id
        FROM new_rows n
        JOIN old_rows o ON o.id = n.id
        WHERE o.is_missing IS DISTINCT FROM n.is_missing
    ), affected AS (
        SELECT DISTINCT ms.item_id
        FROM media_sources ms
        JOIN changed_entries ce ON ce.id = ms.filesystem_entry_id
    ), computed AS (
        SELECT a.item_id,
               CASE WHEN EXISTS (
                   SELECT 1
                   FROM media_sources ms
                   JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                   WHERE ms.item_id = a.item_id
                     AND fe.is_missing = 0
               ) THEN 1 ELSE 0 END AS has_available_source
        FROM affected a
    )
    UPDATE media_items m
    SET has_available_source = c.has_available_source
    FROM computed c
    WHERE m.id = c.item_id
      AND m.has_available_source IS DISTINCT FROM c.has_available_source;
    RETURN NULL;
END;
$$;

CREATE TRIGGER filesystem_entries_availability_au
AFTER UPDATE OF is_missing ON filesystem_entries
REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
FOR EACH STATEMENT
EXECUTE FUNCTION lux_refresh_filesystem_entries_availability_update_stmt();
