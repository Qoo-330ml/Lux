ALTER TABLE users
    ALTER COLUMN has_password DROP DEFAULT;

ALTER TABLE users
    ALTER COLUMN has_password TYPE BIGINT
    USING CASE WHEN has_password THEN 1::BIGINT ELSE 0::BIGINT END;

ALTER TABLE users
    ALTER COLUMN has_password SET DEFAULT 1;

ALTER TABLE users
    ADD CONSTRAINT users_has_password_check CHECK (has_password IN (0, 1));
