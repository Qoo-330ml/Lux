-- The filmography query resolves a person through three independent keys:
-- provider person id, canonical person id and provider identity. Each branch
-- needs its own leading key so the planner does not scan every credit.
CREATE INDEX idx_person_credits_person_lookup
    ON person_credits(person_type, person_id, item_id);
