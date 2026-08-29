ALTER TABLE emby_migration_jobs
ADD COLUMN scope_json TEXT NOT NULL DEFAULT '{"userProfile":true,"libraryAccess":true,"itemState":true,"personFavorites":true}';
