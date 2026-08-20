CREATE TABLE people (
    id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL,
    directory_name TEXT NOT NULL,
    normalized_name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'ACTIVE'
        CHECK (status IN ('ACTIVE', 'MERGED', 'TOMBSTONED')),
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE INDEX idx_people_name
    ON people(normalized_name, display_name);

CREATE TABLE person_id_sequence (
    id BIGSERIAL PRIMARY KEY
);

CREATE TABLE person_identities (
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    match_method TEXT NOT NULL,
    confidence DOUBLE PRECISION,
    evidence_json TEXT NOT NULL DEFAULT '{}',
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    PRIMARY KEY (provider, provider_id)
);

CREATE INDEX idx_person_identities_person
    ON person_identities(person_id, provider, provider_id);

ALTER TABLE person_credits ADD COLUMN lux_person_id TEXT;

CREATE INDEX idx_person_credits_lux_person
    ON person_credits(person_type, lux_person_id);
