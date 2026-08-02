CREATE TABLE server_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

INSERT INTO server_settings (key, value)
VALUES ('resume_played_percent', '90'), ('resume_min_ticks', '1200000000');
