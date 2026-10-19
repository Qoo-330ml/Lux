ALTER TABLE users ADD COLUMN email TEXT;
ALTER TABLE users ADD COLUMN email_verified_at BIGINT;
ALTER TABLE users ADD COLUMN deleted_at BIGINT;
ALTER TABLE users ADD COLUMN deletion_purge_at BIGINT;
ALTER TABLE users ADD COLUMN is_disabled_before_delete BIGINT NOT NULL DEFAULT 0
    CHECK (is_disabled_before_delete IN (0, 1));

CREATE UNIQUE INDEX idx_users_email_case_insensitive
ON users(LOWER(email))
WHERE email IS NOT NULL AND email <> '';
