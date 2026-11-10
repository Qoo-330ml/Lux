-- PostgreSQL half of 0244_metadata_candidate_expiry_index.sql (expired-PENDING candidate cleanup,
-- docs/PERFORMANCE-DB-AUDIT-HLS-SCRAPE-METADATA.md S5/I1). Same statement and same reasoning: the
-- sweep is `WHERE status = 'PENDING' AND expires_at IS NOT NULL AND expires_at < $1
-- ORDER BY expires_at, id LIMIT $2`, so a partial index on exactly the PENDING rows lets the
-- planner read the batch in order instead of sorting the whole backlog. PostgreSQL stores a null
-- ordering term in the index, but the `expires_at IS NOT NULL` predicate keeps the scanned range
-- free of nulls, and the writer only pays for one extra index entry per pending candidate.
CREATE INDEX idx_metadata_candidates_pending_expiry
    ON metadata_candidates(status, expires_at, id)
    WHERE status = 'PENDING';
