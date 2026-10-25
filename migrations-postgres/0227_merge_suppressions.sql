-- Administrator unmerge records the identity key that caused an automatic merge, so the
-- background worker does not merge the same items back within seconds.
--
-- Provider rows cover provider-ID groups (only TMDb participates today, but the column stays
-- generic); title rows cover the normalized-title + year fallback for items without provider IDs.
-- A row is keyed to one library and item type, and identity changes (for example after
-- re-identification) make the stale row harmless because the group no longer matches.
CREATE TABLE merge_suppressions (
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    item_type TEXT NOT NULL,
    provider TEXT,
    provider_id TEXT,
    sort_title TEXT,
    production_year BIGINT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    CHECK (
        (provider IS NOT NULL AND provider_id IS NOT NULL
            AND sort_title IS NULL AND production_year IS NULL)
        OR (provider IS NULL AND provider_id IS NULL
            AND sort_title IS NOT NULL AND production_year IS NOT NULL)
    )
);

CREATE UNIQUE INDEX idx_merge_suppressions_provider_key
    ON merge_suppressions(library_id, item_type, provider, provider_id)
    WHERE provider IS NOT NULL;

CREATE UNIQUE INDEX idx_merge_suppressions_title_key
    ON merge_suppressions(library_id, item_type, sort_title, production_year)
    WHERE sort_title IS NOT NULL;
