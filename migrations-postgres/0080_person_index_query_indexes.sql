CREATE INDEX idx_person_credits_person_item
    ON person_credits(person_type, provider, person_id, item_id);

CREATE INDEX idx_media_items_people_visible
    ON media_items(library_id, id)
    WHERE removed_at IS NULL;
