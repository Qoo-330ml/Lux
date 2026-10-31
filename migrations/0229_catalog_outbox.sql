-- Read-model fan-out queue. Authoritative writes only append here; a separate consumer advances
-- per-name watermarks, so a burst of item changes costs one insert instead of a global counter bump.
CREATE TABLE catalog_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    item_id TEXT NOT NULL,
    library_id TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_catalog_outbox_kind ON catalog_outbox(kind, id);

CREATE TABLE catalog_consumers (
    name TEXT PRIMARY KEY NOT NULL,
    applied_seq INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_error TEXT
);
