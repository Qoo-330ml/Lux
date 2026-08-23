CREATE INDEX IF NOT EXISTS idx_person_credits_person_item
    ON person_credits(person_type, provider, person_id, item_id);
