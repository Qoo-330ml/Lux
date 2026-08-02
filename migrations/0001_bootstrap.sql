CREATE TABLE IF NOT EXISTS lux_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO lux_meta (key, value) VALUES ('schema', 'bootstrap');
