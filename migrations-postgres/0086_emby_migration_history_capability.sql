ALTER TABLE emby_migration_jobs
ADD COLUMN history_capability TEXT NOT NULL DEFAULT 'ITEM_STATE'
CHECK (history_capability IN ('ITEM_STATE', 'EVENT_HISTORY'));
