-- idx_media_items_library_added_visible has had no scans since the current
-- deployment started; the home query uses idx_media_items_library_sort. Drop
-- this redundant 28 MB index to reduce media_items write amplification.
DROP INDEX IF EXISTS idx_media_items_library_added_visible;
