CREATE TABLE collection_sync_outbox (
    collection_provider TEXT NOT NULL,
    collection_provider_id TEXT NOT NULL,
    generation BIGINT NOT NULL DEFAULT 1,
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (collection_provider, collection_provider_id)
);

CREATE INDEX idx_collection_sync_outbox_pending
    ON collection_sync_outbox(updated_at, collection_provider, collection_provider_id);

CREATE TABLE global_collection_work_references (
    collection_provider TEXT NOT NULL,
    collection_provider_id TEXT NOT NULL,
    movie_provider TEXT NOT NULL,
    movie_provider_id TEXT NOT NULL,
    sort_order BIGINT NOT NULL DEFAULT 0,
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

CREATE FUNCTION enqueue_global_collection_sync(
    requested_collection_provider TEXT,
    requested_collection_provider_id TEXT
) RETURNS VOID
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO collection_sync_outbox (
        collection_provider, collection_provider_id, generation, updated_at
    ) VALUES (
        requested_collection_provider, requested_collection_provider_id, 1, unixepoch()
    )
    ON CONFLICT (collection_provider, collection_provider_id) DO UPDATE SET
        generation = collection_sync_outbox.generation + 1,
        updated_at = EXCLUDED.updated_at;
END;
$$;

CREATE FUNCTION queue_collection_item_reference_sync() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM enqueue_global_collection_sync(OLD.collection_provider, OLD.collection_provider_id);
        RETURN OLD;
    ELSIF TG_OP = 'UPDATE' THEN
        PERFORM enqueue_global_collection_sync(OLD.collection_provider, OLD.collection_provider_id);
        IF NEW.collection_provider IS DISTINCT FROM OLD.collection_provider
           OR NEW.collection_provider_id IS DISTINCT FROM OLD.collection_provider_id THEN
            PERFORM enqueue_global_collection_sync(NEW.collection_provider, NEW.collection_provider_id);
        END IF;
        RETURN NEW;
    END IF;

    PERFORM enqueue_global_collection_sync(NEW.collection_provider, NEW.collection_provider_id);
    RETURN NEW;
END;
$$;

CREATE TRIGGER collection_item_references_sync_aud
AFTER INSERT OR UPDATE OR DELETE ON collection_item_references
FOR EACH ROW EXECUTE FUNCTION queue_collection_item_reference_sync();

CREATE FUNCTION queue_collection_media_item_sync() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    linked_identity RECORD;
BEGIN
    IF TG_OP = 'DELETE' THEN
        FOR linked_identity IN
            SELECT DISTINCT collection_provider, collection_provider_id
            FROM collection_item_references
            WHERE item_id = OLD.id
        LOOP
            PERFORM enqueue_global_collection_sync(
                linked_identity.collection_provider,
                linked_identity.collection_provider_id
            );
        END LOOP;
        RETURN OLD;
    END IF;

    IF OLD.removed_at IS DISTINCT FROM NEW.removed_at
       OR OLD.has_available_source IS DISTINCT FROM NEW.has_available_source
       OR OLD.library_id IS DISTINCT FROM NEW.library_id
       OR OLD.item_type IS DISTINCT FROM NEW.item_type THEN
        FOR linked_identity IN
            SELECT DISTINCT collection_provider, collection_provider_id
            FROM collection_item_references
            WHERE item_id = NEW.id
        LOOP
            PERFORM enqueue_global_collection_sync(
                linked_identity.collection_provider,
                linked_identity.collection_provider_id
            );
        END LOOP;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER collection_media_item_sync_au
AFTER UPDATE OF removed_at, has_available_source, library_id, item_type ON media_items
FOR EACH ROW EXECUTE FUNCTION queue_collection_media_item_sync();

CREATE TRIGGER collection_media_item_sync_bd
BEFORE DELETE ON media_items
FOR EACH ROW EXECUTE FUNCTION queue_collection_media_item_sync();

CREATE FUNCTION queue_collection_library_sync() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    linked_identity RECORD;
    target_library_id TEXT;
BEGIN
    target_library_id := CASE WHEN TG_OP = 'DELETE' THEN OLD.id ELSE NEW.id END;
    FOR linked_identity IN
        SELECT DISTINCT reference.collection_provider, reference.collection_provider_id
        FROM media_items item
        JOIN collection_item_references reference ON reference.item_id = item.id
        WHERE item.library_id = target_library_id
    LOOP
        PERFORM enqueue_global_collection_sync(
            linked_identity.collection_provider,
            linked_identity.collection_provider_id
        );
    END LOOP;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END;
$$;

CREATE TRIGGER collection_library_sync_au
AFTER UPDATE OF is_enabled ON libraries
FOR EACH ROW
WHEN (OLD.is_enabled IS DISTINCT FROM NEW.is_enabled)
EXECUTE FUNCTION queue_collection_library_sync();

CREATE TRIGGER collection_library_sync_bd
BEFORE DELETE ON libraries
FOR EACH ROW EXECUTE FUNCTION queue_collection_library_sync();
