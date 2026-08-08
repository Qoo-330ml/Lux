-- Realtime incremental scans are internal event-driven jobs, not scheduled tasks.
UPDATE libraries
SET incremental_schedule = NULL,
    updated_at = unixepoch()
WHERE incremental_schedule IS NOT NULL;

DELETE FROM scheduled_task_configs
WHERE task_type = 'INCREMENTAL_SCAN';

UPDATE scheduled_task_configs
SET task_name = '全量校验媒体库',
    task_description = '按计划校验媒体库索引与文件系统的一致性。',
    source_type = 'SYSTEM',
    plugin_id = NULL
WHERE task_type = 'RECONCILIATION_SCAN';

INSERT INTO scheduled_task_configs (
    owner_type, owner_id, task_type, task_name, task_description,
    source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
)
SELECT
    'LIBRARY',
    l.id,
    'RECONCILIATION_SCAN',
    '全量校验媒体库',
    '按计划校验媒体库索引与文件系统的一致性。',
    'SYSTEM',
    NULL,
    l.reconciliation_schedule,
    CASE WHEN l.reconciliation_schedule IS NULL THEN 0 ELSE 1 END,
    json_object('scanConcurrency', l.scan_concurrency, 'probeConcurrency', l.probe_concurrency)
FROM libraries l
WHERE NOT EXISTS (
    SELECT 1
    FROM scheduled_task_configs s
    WHERE s.owner_type = 'LIBRARY'
      AND s.owner_id = l.id
      AND s.task_type = 'RECONCILIATION_SCAN'
);
