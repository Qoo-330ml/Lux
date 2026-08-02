CREATE TABLE item_images (
    id TEXT PRIMARY KEY NOT NULL,
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    image_type TEXT NOT NULL,
    image_index INTEGER NOT NULL DEFAULT 0,
    local_path TEXT NOT NULL,
    width INTEGER,
    height INTEGER,
    file_size INTEGER,
    content_tag TEXT,
    source TEXT NOT NULL DEFAULT 'LOCAL',
    language TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE (item_id, image_type, image_index)
);

CREATE INDEX idx_item_images_item_id ON item_images(item_id);
