-- Exact ordering index for the recently-added read-model page: the page query orders by
-- (effective_added_at DESC, sort_title, item_id) per library and stops at the page size. Without
-- sort_title in the index PostgreSQL has to sort the whole library slice before applying LIMIT,
-- which made the `on` rail slower than the authoritative query.
CREATE INDEX idx_catalog_items_recent_sort
    ON catalog_items(library_id, effective_added_at DESC, sort_title, item_id);
