CREATE TABLE emby_migration_sources (
    source_base_url TEXT PRIMARY KEY NOT NULL,
    secret_ref TEXT NOT NULL,
    source_label TEXT NOT NULL,
    history_capability TEXT NOT NULL CHECK (history_capability IN ('ITEM_STATE', 'EVENT_HISTORY')),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

ALTER TABLE emby_migration_user_bindings ADD COLUMN secret_ref TEXT;

CREATE INDEX idx_emby_migration_user_bindings_source
ON emby_migration_user_bindings(source_base_url, emby_user_id);
