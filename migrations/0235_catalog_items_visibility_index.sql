-- Covering index for the recently-added rail counts: the available branch is answered from the
-- index alone, and the source-less container branch seeks only the few containers without a source.
CREATE INDEX idx_catalog_items_visibility
    ON catalog_items(library_id, has_available_source, removed_at, merged_into, item_type);
