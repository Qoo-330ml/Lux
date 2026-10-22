-- PostgreSQL search uses contains matching (ILIKE '%query%'); B-tree indexes
-- cannot serve leading wildcards. Trigram GIN indexes cover title, original
-- title and aliases instead of scanning media_search on every search.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS idx_media_search_title_trgm
    ON media_search USING gin (title gin_trgm_ops);

CREATE INDEX IF NOT EXISTS idx_media_search_original_title_trgm
    ON media_search USING gin (original_title gin_trgm_ops);

CREATE INDEX IF NOT EXISTS idx_media_search_aliases_trgm
    ON media_search USING gin (aliases gin_trgm_ops);
