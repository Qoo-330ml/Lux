-- Keep episode FANART indexes dense so the default image endpoint (index 0)
-- always resolves the first persisted artwork.
WITH ranked AS (
    SELECT ii.id,
           ROW_NUMBER() OVER (
               PARTITION BY ii.item_id
               ORDER BY ii.image_index, ii.id
           ) - 1 AS new_index
    FROM item_images AS ii
    JOIN media_items AS mi ON mi.id = ii.item_id
    WHERE mi.item_type = 'EPISODE'
      AND ii.image_type = 'FANART'
)
UPDATE item_images
SET image_index = -1000000000 + (
    SELECT ranked.new_index
    FROM ranked
    WHERE ranked.id = item_images.id
)
WHERE id IN (SELECT id FROM ranked);

WITH ranked AS (
    SELECT ii.id,
           ROW_NUMBER() OVER (
               PARTITION BY ii.item_id
               ORDER BY ii.image_index, ii.id
           ) - 1 AS new_index
    FROM item_images AS ii
    JOIN media_items AS mi ON mi.id = ii.item_id
    WHERE mi.item_type = 'EPISODE'
      AND ii.image_type = 'FANART'
)
UPDATE item_images
SET image_index = (
    SELECT ranked.new_index
    FROM ranked
    WHERE ranked.id = item_images.id
)
WHERE id IN (SELECT id FROM ranked);
