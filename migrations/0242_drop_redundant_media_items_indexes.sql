-- W4 media_items index audit (docs/INDEX-AUDIT.md). Both indexes are a strict left prefix of a
-- wider, non-partial index on the same columns:
--
--   idx_media_items_parent_removed (parent_id, removed_at)        is a prefix of
--   idx_media_items_parent_available (parent_id, removed_at, has_available_source)
--
--   idx_media_items_series_removed (series_id, removed_at)        is a prefix of
--   idx_media_items_series_available (series_id, removed_at, has_available_source)
--
-- No query filters parent_id or series_id without going through the wider index: EXPLAIN QUERY
-- PLAN picks the wider index for the catalog child count, the played-container aggregate and the
-- resume pointer even while the narrow index exists, and dropping the wider index hands every
-- plan over to the narrow one, so the reverse direction is covered too. Keeping both costs one
-- extra index write per media_items insert/update that touches parent_id, series_id or removed_at.
DROP INDEX IF EXISTS idx_media_items_parent_removed;
DROP INDEX IF EXISTS idx_media_items_series_removed;
