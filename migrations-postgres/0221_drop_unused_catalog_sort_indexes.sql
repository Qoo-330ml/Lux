-- These optional sort indexes have had no scans on the current deployment.
-- Keeping them costs write amplification on every media_items update; the
-- catalog still has the default date/title index for the hot path.
DROP INDEX IF EXISTS idx_media_items_library_premiere_sort;
DROP INDEX IF EXISTS idx_media_items_library_rating_sort;
