-- LUX-830: expression indexes for the default catalog sorts.
-- PostgreSQL mirrors the SQLite expressions; partial indexes keep the same
-- removed-at semantics.
CREATE INDEX IF NOT EXISTS idx_media_items_library_name_sort
    ON media_items(library_id, lower(title), id)
    WHERE removed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_library_sortname_sort
    ON media_items(library_id, lower(sort_title), id)
    WHERE removed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_library_added_sort
    ON media_items(library_id, added_at DESC, lower(title), id)
    WHERE removed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_library_premiere_sort
    ON media_items(
        library_id,
        (CASE WHEN NULLIF(premiere_date, '') IS NULL AND production_year IS NULL THEN 1 ELSE 0 END),
        (COALESCE(NULLIF(premiere_date, ''), CAST(production_year AS TEXT) || '-01-01')) DESC,
        lower(title),
        id
    )
    WHERE removed_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_media_items_library_rating_sort
    ON media_items(
        library_id,
        (CASE WHEN rating IS NULL THEN 1 ELSE 0 END) ASC,
        rating DESC,
        lower(title),
        id
    )
    WHERE removed_at IS NULL;
