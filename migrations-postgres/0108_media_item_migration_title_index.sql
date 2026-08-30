-- sort_title is the scanner's lower-cased title key.  Keep the migration
-- fallback lookup index-leading so title-only pages do not scan every library
-- row before applying the production-year and visibility predicates.
CREATE INDEX idx_media_items_migration_title
    ON media_items(item_type, sort_title, production_year, library_id, id)
    WHERE removed_at IS NULL;
