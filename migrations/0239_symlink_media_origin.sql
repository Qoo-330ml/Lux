ALTER TABLE scan_manifest_entries
ADD COLUMN is_symlink INTEGER NOT NULL DEFAULT 0 CHECK (is_symlink IN (0, 1));

ALTER TABLE scan_manifest_entries
ADD COLUMN resolved_path TEXT;

ALTER TABLE filesystem_entries
ADD COLUMN is_symlink INTEGER NOT NULL DEFAULT 0 CHECK (is_symlink IN (0, 1));

ALTER TABLE filesystem_entries
ADD COLUMN resolved_path TEXT;
