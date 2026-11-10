-- Expired-PENDING candidate cleanup (docs/PERFORMANCE-DB-AUDIT-HLS-SCRAPE-METADATA.md S5/I1).
-- The sweep orders PENDING candidates by `expires_at` and takes one batch at a time, but no index
-- carried `expires_at`: the planner seeked `idx_metadata_candidates_status (status=?)` and then
-- collected every PENDING row into a temp B-tree before the LIMIT could apply, so the cost grew
-- with the whole backlog instead of with the batch.
--
-- The index is partial because the sweep only ever reads PENDING rows (`status` is also fixed by
-- the partial predicate, so the three columns are exactly the seek key plus the order the LIMIT
-- follows). Candidates in any other status carry no index entry at all, and a status transition
-- away from PENDING removes it; the candidate writer keeps one extra index entry per pending row
-- in exchange. PostgreSQL half: 0246_metadata_candidate_expiry_index.sql.
CREATE INDEX idx_metadata_candidates_pending_expiry
    ON metadata_candidates(status, expires_at, id)
    WHERE status = 'PENDING';
