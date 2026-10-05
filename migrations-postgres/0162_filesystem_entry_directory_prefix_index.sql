-- Local-metadata batches resolve the sources of every referenced directory with
-- `relative_path LIKE 'dir/%' AND relative_path NOT LIKE 'dir/%/%'`. The unique
-- (library_root_id, relative_path) index is built with the database collation (for example
-- en_US.utf8), which cannot serve LIKE prefix scans, so each batch walked the whole table.
-- `text_pattern_ops` lets PostgreSQL turn the prefix into an index range.
CREATE INDEX IF NOT EXISTS idx_filesystem_entries_dir_prefix
    ON filesystem_entries(library_root_id, relative_path text_pattern_ops)
    WHERE entry_kind = 'FILE' AND is_missing = 0;
