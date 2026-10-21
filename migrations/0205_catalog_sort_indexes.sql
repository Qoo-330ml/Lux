-- LUX-830: expression indexes for the default catalog sorts.
-- Every index is partial on visible rows so scans and the planner keep the
-- existing removed-at filtering semantics.
CREATE INDEX IF NOT EXISTS idx_media_items_library_name_sort
    ON media_items(library_id, LOWER(title), id)
    WHERE removed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_library_sortname_sort
    ON media_items(library_id, LOWER(sort_title), id)
    WHERE removed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_library_added_sort
    ON media_items(library_id, added_at DESC, LOWER(title), id)
    WHERE removed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_library_premiere_sort
    ON media_items(
        library_id,
        (CASE WHEN NULLIF(premiere_date, '') IS NULL AND production_year IS NULL THEN 1 ELSE 0 END),
        (COALESCE(NULLIF(premiere_date, ''), CAST(production_year AS TEXT) || '-01-01')) DESC,
        LOWER(title),
        id
    )
    WHERE removed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_library_rating_sort
    ON media_items(
        library_id,
        (CASE WHEN rating IS NULL THEN 1 ELSE 0 END) ASC,
        rating DESC,
        LOWER(title),
        id
    )
    WHERE removed_at IS NULL;
