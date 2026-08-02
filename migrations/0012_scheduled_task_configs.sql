CREATE TABLE scheduled_task_configs (
    owner_type TEXT NOT NULL CHECK (owner_type IN ('GLOBAL', 'LIBRARY')),
    owner_id TEXT NOT NULL,
    task_type TEXT NOT NULL,
    cron_or_interval TEXT,
    is_enabled INTEGER NOT NULL DEFAULT 1 CHECK (is_enabled IN (0, 1)),
    resource_limit_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (owner_type, owner_id, task_type)
);

CREATE INDEX idx_scheduled_task_configs_owner ON scheduled_task_configs(owner_type, owner_id);
