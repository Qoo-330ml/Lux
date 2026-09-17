DROP TRIGGER IF EXISTS filesystem_entries_availability_au ON filesystem_entries;

CREATE TRIGGER filesystem_entries_availability_au
AFTER UPDATE OF is_missing ON filesystem_entries
FOR EACH ROW
WHEN (OLD.is_missing IS DISTINCT FROM NEW.is_missing)
EXECUTE FUNCTION lux_refresh_item_availability();
