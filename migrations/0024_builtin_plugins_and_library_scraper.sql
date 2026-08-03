CREATE TABLE installed_plugins (
    plugin_id TEXT PRIMARY KEY NOT NULL,
    is_enabled INTEGER NOT NULL DEFAULT 1 CHECK (is_enabled IN (0, 1)),
    installed_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

ALTER TABLE libraries ADD COLUMN scraper_id TEXT;
