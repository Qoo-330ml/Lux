ALTER TABLE item_images ADD COLUMN source_url TEXT;

CREATE INDEX idx_item_images_source_url
    ON item_images(item_id, image_type, source_url);
