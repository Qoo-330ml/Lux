ALTER TABLE emby_migration_jobs
ADD COLUMN emby_user_ids_json TEXT NOT NULL DEFAULT '[]';
