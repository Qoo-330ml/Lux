CREATE INDEX idx_filesystem_entries_missing_fingerprint_cleanup
    ON filesystem_entries(library_root_id, relative_path)
    WHERE entry_kind = 'FILE' AND is_missing = 1 AND fingerprint IS NOT NULL;

CREATE INDEX idx_filesystem_entries_missing_retention_cleanup
    ON filesystem_entries(updated_at, id)
    WHERE entry_kind = 'FILE' AND is_missing = 1;

UPDATE scan_fingerprint_migration_state
SET completed_at = NULL, updated_at = extract(epoch FROM now())::BIGINT
WHERE EXISTS (
    SELECT 1 FROM filesystem_entries fe
    JOIN library_roots lr ON lr.id = fe.library_root_id
    WHERE lr.library_id = scan_fingerprint_migration_state.library_id
      AND fe.entry_kind = 'FILE' AND fe.is_missing = 1 AND fe.fingerprint IS NOT NULL
);

CREATE OR REPLACE FUNCTION lux_clear_missing_file_fingerprint()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.entry_kind = 'FILE' AND NEW.is_missing = 1 THEN
        NEW.fingerprint := NULL;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_filesystem_entries_clear_missing_fingerprint
BEFORE INSERT OR UPDATE OF is_missing, fingerprint ON filesystem_entries
FOR EACH ROW
EXECUTE FUNCTION lux_clear_missing_file_fingerprint();
