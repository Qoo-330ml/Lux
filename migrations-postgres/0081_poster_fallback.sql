ALTER TABLE media_items
RENAME COLUMN thumbnail_fallback_required TO poster_fallback_required;

INSERT INTO item_images (
    id, item_id, image_type, image_index, local_path, width, height,
    file_size, content_tag, source, language, created_at, updated_at
)
SELECT substr(md5(id || '-poster'), 1, 8) || '-' ||
       substr(md5(id || '-poster'), 9, 4) || '-' ||
       substr(md5(id || '-poster'), 13, 4) || '-' ||
       substr(md5(id || '-poster'), 17, 4) || '-' ||
       substr(md5(id || '-poster'), 21, 12), item_id, 'POSTER', image_index, local_path, width, height,
       file_size, content_tag, source, language, created_at, unixepoch()
FROM item_images AS thumbnail
WHERE thumbnail.image_type = 'THUMB'
  AND thumbnail.source = 'STRM_FFMPEG'
  AND NOT EXISTS (
      SELECT 1
      FROM item_images AS poster
      WHERE poster.item_id = thumbnail.item_id
        AND poster.image_type = 'POSTER'
        AND poster.image_index = thumbnail.image_index
  );

UPDATE media_items
SET poster_fallback_required = CASE WHEN EXISTS (
    SELECT 1
    FROM media_sources ms
    JOIN item_images ii ON ii.item_id = media_items.id
    WHERE ms.item_id = media_items.id
      AND ms.source_kind = 'STRM_URL'
      AND ii.image_type IN ('POSTER', 'THUMB')
      AND ii.image_index = 0
) THEN 0 ELSE 1 END
WHERE EXISTS (
    SELECT 1
    FROM media_sources ms
    WHERE ms.item_id = media_items.id
      AND ms.source_kind = 'STRM_URL'
);
