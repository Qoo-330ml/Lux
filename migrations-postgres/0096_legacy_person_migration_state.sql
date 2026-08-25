CREATE TABLE legacy_person_migration_state (
    id BIGINT PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'COMPLETED')),
    schema_version BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
