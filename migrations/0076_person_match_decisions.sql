ALTER TABLE person_match_candidates ADD COLUMN target_person_id TEXT;
ALTER TABLE person_match_candidates ADD COLUMN previous_person_id TEXT;

CREATE INDEX idx_person_match_candidates_target
    ON person_match_candidates(target_person_id, status, updated_at);
