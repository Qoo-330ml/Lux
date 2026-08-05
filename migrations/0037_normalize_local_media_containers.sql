-- Emby exposes the playable file extension as Container. Keep local sources
-- aligned with their actual path instead of ffprobe's compound format_name.
WITH RECURSIVE extension_scan(entry_id, rest, extension) AS (
    SELECT id, relative_path, NULL
    FROM filesystem_entries

    UNION ALL

    SELECT entry_id,
           substr(rest, instr(rest, '.') + 1),
           substr(rest, instr(rest, '.') + 1)
    FROM extension_scan
    WHERE instr(rest, '.') > 0
),
extensions AS (
    SELECT entry_id, lower(extension) AS container
    FROM extension_scan
    WHERE instr(rest, '.') = 0 AND extension IS NOT NULL
)
UPDATE media_sources
SET container = (
    SELECT extensions.container
    FROM extensions
    WHERE extensions.entry_id = media_sources.filesystem_entry_id
)
WHERE source_kind = 'LOCAL_FILE'
  AND filesystem_entry_id IS NOT NULL
  AND EXISTS (
      SELECT 1
      FROM extensions
      WHERE extensions.entry_id = media_sources.filesystem_entry_id
  );
