CREATE INDEX IF NOT EXISTS idx_person_credits_person_item
    ON person_credits(person_type, provider, person_id, item_id);

CREATE INDEX IF NOT EXISTS idx_media_items_people_visible_v2
    ON media_items(library_id, id)
    WHERE removed_at IS NULL;
