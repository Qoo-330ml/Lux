CREATE TABLE collection_sync_outbox (
    collection_provider TEXT NOT NULL,
    collection_provider_id TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 1,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (collection_provider, collection_provider_id)
);

CREATE INDEX idx_collection_sync_outbox_pending
    ON collection_sync_outbox(updated_at, collection_provider, collection_provider_id);

CREATE TABLE global_collection_work_references (
    collection_provider TEXT NOT NULL,
    collection_provider_id TEXT NOT NULL,
    movie_provider TEXT NOT NULL,
    movie_provider_id TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (
        collection_provider, collection_provider_id,
        movie_provider, movie_provider_id
    )
);

CREATE INDEX idx_global_collection_work_references_work
    ON global_collection_work_references(movie_provider, movie_provider_id);

INSERT INTO collection_sync_outbox (collection_provider, collection_provider_id)
SELECT DISTINCT collection_provider, collection_provider_id
FROM collection_item_references;

CREATE TRIGGER collection_item_references_sync_ai
AFTER INSERT ON collection_item_references
BEGIN
    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    ) VALUES (NEW.collection_provider, NEW.collection_provider_id, 1, unixepoch())
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = excluded.updated_at;
END;

CREATE TRIGGER collection_item_references_sync_au
AFTER UPDATE OF item_id, movie_provider, movie_provider_id,
    collection_provider, collection_provider_id, collection_title, sort_order
ON collection_item_references
BEGIN
    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    ) VALUES (OLD.collection_provider, OLD.collection_provider_id, 1, unixepoch())
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = excluded.updated_at;

    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    )
    SELECT NEW.collection_provider, NEW.collection_provider_id, 1, unixepoch()
    WHERE NEW.collection_provider <> OLD.collection_provider
       OR NEW.collection_provider_id <> OLD.collection_provider_id
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = excluded.updated_at;
END;

CREATE TRIGGER collection_item_references_sync_ad
AFTER DELETE ON collection_item_references
BEGIN
    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    ) VALUES (OLD.collection_provider, OLD.collection_provider_id, 1, unixepoch())
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = excluded.updated_at;
END;

CREATE TRIGGER collection_media_item_sync_au
AFTER UPDATE OF removed_at, has_available_source, library_id, item_type ON media_items
WHEN OLD.removed_at IS NOT NEW.removed_at
  OR OLD.has_available_source IS NOT NEW.has_available_source
  OR OLD.library_id IS NOT NEW.library_id
  OR OLD.item_type IS NOT NEW.item_type
BEGIN
    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    )
    SELECT DISTINCT reference.collection_provider, reference.collection_provider_id,
           1, unixepoch()
    FROM collection_item_references reference
    WHERE reference.item_id = NEW.id
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = excluded.updated_at;
END;

CREATE TRIGGER collection_media_item_sync_bd
BEFORE DELETE ON media_items
BEGIN
    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    )
    SELECT DISTINCT reference.collection_provider, reference.collection_provider_id,
           1, unixepoch()
    FROM collection_item_references reference
    WHERE reference.item_id = OLD.id
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = excluded.updated_at;
END;

CREATE TRIGGER collection_library_sync_au
AFTER UPDATE OF is_enabled ON libraries
WHEN OLD.is_enabled IS NOT NEW.is_enabled
BEGIN
    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    )
    SELECT DISTINCT reference.collection_provider, reference.collection_provider_id,
           1, unixepoch()
    FROM media_items item
    JOIN collection_item_references reference ON reference.item_id = item.id
    WHERE item.library_id = NEW.id
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = excluded.updated_at;
END;

CREATE TRIGGER collection_library_sync_bd
BEFORE DELETE ON libraries
BEGIN
    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    )
    SELECT DISTINCT reference.collection_provider, reference.collection_provider_id,
           1, unixepoch()
    FROM media_items item
    JOIN collection_item_references reference ON reference.item_id = item.id
    WHERE item.library_id = OLD.id
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = excluded.updated_at;
END;
