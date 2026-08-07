ALTER TABLE scheduled_task_configs
    ADD COLUMN task_name TEXT NOT NULL DEFAULT '';

ALTER TABLE scheduled_task_configs
    ADD COLUMN task_description TEXT NOT NULL DEFAULT '';

ALTER TABLE scheduled_task_configs
    ADD COLUMN source_type TEXT NOT NULL DEFAULT 'SYSTEM';

ALTER TABLE scheduled_task_configs
    ADD COLUMN plugin_id TEXT;

UPDATE scheduled_task_configs
SET task_name = CASE task_type
    WHEN 'INCREMENTAL_SCAN' THEN '扫描媒体文件夹'
    WHEN 'RECONCILIATION_SCAN' THEN '全量校验媒体库'
    WHEN 'METADATA_PARSE' THEN '元数据刮削'
    ELSE task_type
END,
task_description = CASE task_type
    WHEN 'INCREMENTAL_SCAN' THEN '按计划检查媒体库根路径中的新增和变更文件。'
    WHEN 'RECONCILIATION_SCAN' THEN '按计划校验媒体库索引与文件系统的一致性。'
    WHEN 'METADATA_PARSE' THEN '解析本地元数据，并在已配置时调用刮削插件补全内容。'
    ELSE '由 Lux 后台执行的注册任务。'
END
WHERE task_name = '';

UPDATE scheduled_task_configs
SET plugin_id = CASE
        WHEN task_type = 'METADATA_PARSE' THEN (
            SELECT scraper_id
            FROM libraries
            WHERE libraries.id = scheduled_task_configs.owner_id
        )
        ELSE NULL
    END,
    source_type = CASE
        WHEN task_type = 'METADATA_PARSE'
             AND EXISTS (
                 SELECT 1
                 FROM libraries
                 WHERE libraries.id = scheduled_task_configs.owner_id
                   AND libraries.scraper_id IS NOT NULL
             ) THEN 'PLUGIN'
        ELSE 'SYSTEM'
    END
WHERE owner_type = 'LIBRARY';

INSERT INTO scheduled_task_configs (
    owner_type, owner_id, task_type, task_name, task_description,
    source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
)
SELECT
    'LIBRARY',
    l.id,
    'INCREMENTAL_SCAN',
    '扫描媒体文件夹',
    '按计划检查媒体库根路径中的新增和变更文件。',
    'SYSTEM',
    NULL,
    l.incremental_schedule,
    CASE WHEN l.incremental_schedule IS NULL THEN 0 ELSE 1 END,
    json_object('scanConcurrency', l.scan_concurrency, 'probeConcurrency', l.probe_concurrency)
FROM libraries l
WHERE NOT EXISTS (
    SELECT 1
    FROM scheduled_task_configs s
    WHERE s.owner_type = 'LIBRARY'
      AND s.owner_id = l.id
      AND s.task_type = 'INCREMENTAL_SCAN'
);

INSERT INTO scheduled_task_configs (
    owner_type, owner_id, task_type, task_name, task_description,
    source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
)
SELECT
    'LIBRARY',
    l.id,
    'METADATA_PARSE',
    '元数据刮削',
    '解析本地元数据，并在已配置时调用刮削插件补全内容。',
    CASE WHEN l.scraper_id IS NULL THEN 'SYSTEM' ELSE 'PLUGIN' END,
    l.scraper_id,
    l.metadata_schedule,
    CASE WHEN l.metadata_schedule IS NULL THEN 0 ELSE 1 END,
    '{}'
FROM libraries l
WHERE NOT EXISTS (
    SELECT 1
    FROM scheduled_task_configs s
    WHERE s.owner_type = 'LIBRARY'
      AND s.owner_id = l.id
      AND s.task_type = 'METADATA_PARSE'
);
