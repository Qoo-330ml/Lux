-- W4 media_items index audit (docs/INDEX-AUDIT.md), PostgreSQL half of
-- 0242_drop_redundant_media_items_indexes.sql. Both indexes are a strict left prefix of a wider,
-- non-partial index on the same columns:
--
--   idx_media_items_parent_removed (parent_id, removed_at)  ->  idx_media_items_parent_available
--   idx_media_items_series_removed (series_id, removed_at)  ->  idx_media_items_series_available
--
-- The wider index answers every equality/range predicate the narrow one answers, so nothing is
-- lost except one index write per media_items insert/update that touches parent_id, series_id or
-- removed_at. PostgreSQL has no per-index scan counters here; the SQLite side carries the plan
-- evidence (`scripts/audit-media-items-indexes.py`) and the audit doc records what still needs a
-- NAS measurement.
DROP INDEX IF EXISTS idx_media_items_parent_removed;
DROP INDEX IF EXISTS idx_media_items_series_removed;
