CREATE TABLE metadata_candidates (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    candidate_json TEXT NOT NULL,
    score REAL NOT NULL CHECK (score >= 0 AND score <= 100),
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'SELECTED', 'REJECTED', 'EXPIRED')),
    expires_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_metadata_candidates_status ON metadata_candidates(status, created_at, id);
CREATE INDEX idx_metadata_candidates_item ON metadata_candidates(item_id, status, created_at, id);
