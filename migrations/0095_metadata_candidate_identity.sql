UPDATE metadata_candidates
SET status = 'REJECTED', updated_at = unixepoch()
WHERE id IN (
    SELECT id
    FROM (
        SELECT id,
               ROW_NUMBER() OVER (
                   PARTITION BY item_id, provider, provider_id
                   ORDER BY score DESC, created_at DESC, id DESC
               ) AS duplicate_rank
        FROM metadata_candidates
        WHERE status = 'PENDING'
    )
    WHERE duplicate_rank > 1
);

CREATE UNIQUE INDEX idx_metadata_candidates_pending_identity
    ON metadata_candidates(item_id, provider, provider_id)
    WHERE status = 'PENDING';
