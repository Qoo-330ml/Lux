CREATE TABLE person_credits (
    item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    person_id TEXT NOT NULL,
    person_type TEXT NOT NULL,
    person_name TEXT NOT NULL,
    provider TEXT NOT NULL DEFAULT '',
    role TEXT NOT NULL DEFAULT '',
    sort_order BIGINT NOT NULL DEFAULT 0,
    biography TEXT,
    birthday TEXT,
    deathday TEXT,
    known_for_department TEXT,
    place_of_birth TEXT,
    PRIMARY KEY (item_id, person_type, provider, person_id, role)
);

CREATE INDEX idx_person_credits_item
    ON person_credits(item_id);

CREATE INDEX idx_person_credits_person
    ON person_credits(person_type, provider, person_id);
