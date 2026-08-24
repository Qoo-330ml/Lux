CREATE TABLE library_scrapers (
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    scraper_id TEXT NOT NULL,
    position BIGINT NOT NULL CHECK (position >= 0),
    role TEXT NOT NULL CHECK (role IN ('PRIMARY', 'SUPPLEMENT', 'BACKUP', 'BOTH')),
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (library_id, scraper_id),
    UNIQUE (library_id, position)
);

CREATE INDEX idx_library_scrapers_position
    ON library_scrapers(library_id, position);

INSERT INTO library_scrapers (library_id, scraper_id, position, role)
SELECT id, scraper_id, 0, 'PRIMARY'
FROM libraries
WHERE scraper_id IS NOT NULL AND length(trim(scraper_id)) > 0;
