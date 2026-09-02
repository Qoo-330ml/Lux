DROP INDEX IF EXISTS idx_filesystem_entries_root_path;

INSERT INTO lux_meta (key, value)
VALUES ('database_lifecycle_cleanup_v1', 'PENDING')
ON CONFLICT (key) DO NOTHING;
