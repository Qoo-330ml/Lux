CREATE TABLE scheduled_task_plans (
    id TEXT PRIMARY KEY NOT NULL,
    task_type TEXT NOT NULL,
    plan_name TEXT NOT NULL,
    task_name TEXT NOT NULL,
    task_description TEXT NOT NULL,
    source_type TEXT NOT NULL CHECK (source_type IN ('SYSTEM', 'PLUGIN')),
    plugin_id TEXT,
    cron_or_interval TEXT,
    is_enabled BIGINT NOT NULL DEFAULT 1 CHECK (is_enabled IN (0, 1)),
    resource_limit_json TEXT NOT NULL DEFAULT '{}',
    scope_type TEXT NOT NULL CHECK (scope_type IN ('GLOBAL', 'LIBRARY')),
    is_default BIGINT NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_scheduled_task_plans_type
    ON scheduled_task_plans(task_type, updated_at, id);

CREATE TABLE scheduled_task_plan_libraries (
    plan_id TEXT NOT NULL REFERENCES scheduled_task_plans(id) ON DELETE CASCADE,
    library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (plan_id, library_id)
);

CREATE INDEX idx_scheduled_task_plan_libraries_library
    ON scheduled_task_plan_libraries(library_id, plan_id);

ALTER TABLE scheduled_task_configs
    ADD COLUMN plan_id TEXT REFERENCES scheduled_task_plans(id) ON DELETE SET NULL;

INSERT INTO scheduled_task_plans (
    id, task_type, plan_name, task_name, task_description,
    source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json,
    scope_type, is_default
)
SELECT
    md5('lux-plan:' || task_type || ':' || task_name || ':' || task_description || ':' ||
        source_type || ':' || coalesce(plugin_id, '') || ':' ||
        coalesce(cron_or_interval, '') || ':' || is_enabled || ':' || resource_limit_json),
    task_type,
    task_name,
    task_name,
    task_description,
    source_type,
    plugin_id,
    cron_or_interval,
    is_enabled,
    resource_limit_json,
    'LIBRARY',
    0
FROM scheduled_task_configs
WHERE owner_type = 'LIBRARY'
GROUP BY task_type, task_name, task_description, source_type, plugin_id,
         cron_or_interval, is_enabled, resource_limit_json;

UPDATE scheduled_task_plans p
SET is_default = 1
WHERE p.id IN (
    SELECT MIN(id)
    FROM scheduled_task_plans
    WHERE scope_type = 'LIBRARY'
    GROUP BY task_type, source_type, plugin_id
);

UPDATE scheduled_task_configs s
SET plan_id = (
    SELECT p.id
    FROM scheduled_task_plans p
    WHERE p.scope_type = 'LIBRARY'
      AND p.task_type = s.task_type
      AND p.task_name = s.task_name
      AND p.task_description = s.task_description
      AND p.source_type = s.source_type
      AND p.plugin_id IS NOT DISTINCT FROM s.plugin_id
      AND p.cron_or_interval IS NOT DISTINCT FROM s.cron_or_interval
      AND p.is_enabled = s.is_enabled
      AND p.resource_limit_json = s.resource_limit_json
    LIMIT 1
)
WHERE s.owner_type = 'LIBRARY';

INSERT INTO scheduled_task_plan_libraries (plan_id, library_id)
SELECT plan_id, owner_id
FROM scheduled_task_configs
WHERE owner_type = 'LIBRARY' AND plan_id IS NOT NULL;

INSERT INTO scheduled_task_plans (
    id, task_type, plan_name, task_name, task_description,
    source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json,
    scope_type, is_default
)
SELECT
    md5('lux-global-plan:' || task_type || ':' || task_name || ':' || task_description || ':' ||
        source_type || ':' || coalesce(plugin_id, '') || ':' ||
        coalesce(cron_or_interval, '') || ':' || is_enabled || ':' || resource_limit_json),
    task_type,
    task_name,
    task_name,
    task_description,
    source_type,
    plugin_id,
    cron_or_interval,
    is_enabled,
    resource_limit_json,
    'GLOBAL',
    1
FROM scheduled_task_configs
WHERE owner_type = 'GLOBAL'
GROUP BY task_type, task_name, task_description, source_type, plugin_id,
         cron_or_interval, is_enabled, resource_limit_json;

UPDATE scheduled_task_configs s
SET plan_id = (
    SELECT p.id
    FROM scheduled_task_plans p
    WHERE p.scope_type = 'GLOBAL'
      AND p.task_type = s.task_type
      AND p.task_name = s.task_name
      AND p.task_description = s.task_description
      AND p.source_type = s.source_type
      AND p.plugin_id IS NOT DISTINCT FROM s.plugin_id
      AND p.cron_or_interval IS NOT DISTINCT FROM s.cron_or_interval
      AND p.is_enabled = s.is_enabled
      AND p.resource_limit_json = s.resource_limit_json
    LIMIT 1
)
WHERE s.owner_type = 'GLOBAL';
