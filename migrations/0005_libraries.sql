CREATE TABLE libraries (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('MOVIE', 'SERIES', 'MIXED')),
    is_enabled INTEGER NOT NULL DEFAULT 1 CHECK (is_enabled IN (0, 1)),
    realtime_watch_enabled INTEGER NOT NULL DEFAULT 0 CHECK (realtime_watch_enabled IN (0, 1)),
    incremental_schedule TEXT,
    reconciliation_schedule TEXT,
    metadata_schedule TEXT,
    scan_concurrency INTEGER NOT NULL DEFAULT 2 CHECK (scan_concurrency > 0),
    probe_concurrency INTEGER NOT NULL DEFAULT 1 CHECK (probe_concurrency > 0),
    last_scan_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE library_roots (
    id TEXT PRIMARY KEY NOT NULL,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    canonical_path TEXT NOT NULL,
    display_path TEXT NOT NULL,
    is_available INTEGER NOT NULL CHECK (is_available IN (0, 1)),
    is_writable INTEGER NOT NULL CHECK (is_writable IN (0, 1)),
    last_checked_at INTEGER NOT NULL DEFAULT (unixepoch()),
    unavailable_since INTEGER,
    scan_cursor TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (library_id, canonical_path)
);

CREATE INDEX idx_library_roots_library_id ON library_roots(library_id);
CREATE INDEX idx_library_roots_canonical_path ON library_roots(canonical_path);
