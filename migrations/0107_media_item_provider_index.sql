CREATE TABLE media_item_provider_ids (
    media_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    item_type TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    PRIMARY KEY (media_item_id, provider, provider_id)
);

CREATE INDEX idx_media_item_provider_ids_lookup
    ON media_item_provider_ids(item_type, provider, provider_id, media_item_id);

INSERT INTO media_item_provider_ids (media_item_id, item_type, provider, provider_id)
SELECT mi.id, mi.item_type, lower(json_each.key), json_each.value
FROM media_items AS mi
JOIN json_each(
    CASE
        WHEN json_valid(mi.provider_ids_json) THEN mi.provider_ids_json
        ELSE '{}'
    END
) AS json_each
WHERE json_each.type = 'text';

CREATE TRIGGER media_item_provider_ids_ai
AFTER INSERT ON media_items
BEGIN
    INSERT OR IGNORE INTO media_item_provider_ids
        (media_item_id, item_type, provider, provider_id)
    SELECT NEW.id, NEW.item_type, lower(json_each.key), json_each.value
    FROM json_each(
        CASE
            WHEN json_valid(NEW.provider_ids_json) THEN NEW.provider_ids_json
            ELSE '{}'
        END
    ) AS json_each
    WHERE json_each.type = 'text';
END;

CREATE TRIGGER media_item_provider_ids_au
AFTER UPDATE OF item_type, provider_ids_json ON media_items
BEGIN
    DELETE FROM media_item_provider_ids WHERE media_item_id = NEW.id;
    INSERT OR IGNORE INTO media_item_provider_ids
        (media_item_id, item_type, provider, provider_id)
    SELECT NEW.id, NEW.item_type, lower(json_each.key), json_each.value
    FROM json_each(
        CASE
            WHEN json_valid(NEW.provider_ids_json) THEN NEW.provider_ids_json
            ELSE '{}'
        END
    ) AS json_each
    WHERE json_each.type = 'text';
END;
