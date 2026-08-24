CREATE TABLE person_manifest_restore_state (
    id BIGINT PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('PENDING', 'COMPLETED')),
    schema_version BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

CREATE TABLE person_manifest_index_state (
    person_id TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    manifest_checksum TEXT NOT NULL,
    manifest_schema_version BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);
