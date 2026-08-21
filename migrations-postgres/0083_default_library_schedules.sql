UPDATE libraries
SET reconciliation_schedule = '0 3 * * 0',
    updated_at = EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
WHERE reconciliation_schedule IS NULL;

UPDATE scheduled_task_configs
SET cron_or_interval = '0 3 * * 0',
    is_enabled = 1,
    updated_at = EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
WHERE owner_type = 'LIBRARY'
  AND task_type = 'RECONCILIATION_SCAN'
  AND cron_or_interval IS NULL;

UPDATE libraries
SET metadata_schedule = '0 4 * * 0',
    updated_at = EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
WHERE metadata_schedule IS NULL
  AND scraper_id IS NOT NULL;

UPDATE scheduled_task_configs
SET cron_or_interval = '0 4 * * 0',
    is_enabled = 1,
    source_type = 'PLUGIN',
    plugin_id = (
        SELECT scraper_id
        FROM libraries
        WHERE libraries.id = scheduled_task_configs.owner_id
    ),
    updated_at = EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT
WHERE owner_type = 'LIBRARY'
  AND task_type = 'METADATA_PARSE'
  AND cron_or_interval IS NULL
  AND EXISTS (
      SELECT 1
      FROM libraries
      WHERE libraries.id = scheduled_task_configs.owner_id
        AND libraries.scraper_id IS NOT NULL
  );
