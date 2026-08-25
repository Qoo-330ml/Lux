CREATE TABLE legacy_person_migration_state (
    id INTEGER PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'COMPLETED')),
    schema_version INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
