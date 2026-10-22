CREATE TABLE user_registration_requests (
    id TEXT PRIMARY KEY NOT NULL
        CHECK (length(btrim(id)) BETWEEN 1 AND 128),
    recipient_email TEXT NOT NULL
        CHECK (
            length(recipient_email) BETWEEN 3 AND 254
            AND recipient_email = btrim(recipient_email)
            AND recipient_email = lower(recipient_email)
        ),
    username_normalized TEXT NOT NULL
        CHECK (length(btrim(username_normalized)) BETWEEN 1 AND 128),
    display_name TEXT NOT NULL
        CHECK (length(btrim(display_name)) BETWEEN 1 AND 128),
    password_hash TEXT
        CHECK (
            password_hash IS NULL
            OR (length(password_hash) BETWEEN 10 AND 2048 AND left(password_hash, 10) = '$argon2id$')
        ),
    permission_snapshot_json TEXT NOT NULL
        CHECK (
            octet_length(permission_snapshot_json) <= 32768
            AND jsonb_typeof(permission_snapshot_json::jsonb) = 'object'
        ),
    status TEXT NOT NULL DEFAULT 'PENDING_EMAIL'
        CHECK (status IN ('PENDING_EMAIL', 'PENDING_APPROVAL', 'APPROVED', 'REJECTED', 'EXPIRED')),
    expires_at BIGINT NOT NULL CHECK (expires_at >= 0),
    email_verified_at BIGINT,
    reviewed_at BIGINT,
    reviewed_by_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    approved_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    last_error_code TEXT
        CHECK (last_error_code IS NULL OR last_error_code IN (
            'EMAIL_CONFLICT', 'USERNAME_CONFLICT', 'MAIL_DELIVERY_FAILED', 'PERMISSIONS_UNAVAILABLE'
        )),
    created_at BIGINT NOT NULL DEFAULT (unixepoch()) CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()) CHECK (updated_at >= created_at),
    CHECK (expires_at > created_at),
    CHECK (email_verified_at IS NULL OR (email_verified_at >= created_at AND email_verified_at < expires_at)),
    CHECK (reviewed_at IS NULL OR reviewed_at >= COALESCE(email_verified_at, created_at)),
    CHECK (updated_at >= COALESCE(reviewed_at, email_verified_at, created_at)),
    CHECK (
        (status = 'PENDING_EMAIL' AND email_verified_at IS NULL AND reviewed_at IS NULL
            AND reviewed_by_user_id IS NULL AND approved_user_id IS NULL AND password_hash IS NOT NULL)
        OR (status = 'PENDING_APPROVAL' AND email_verified_at IS NOT NULL AND reviewed_at IS NULL
            AND reviewed_by_user_id IS NULL AND approved_user_id IS NULL AND password_hash IS NOT NULL)
        OR (status = 'APPROVED' AND email_verified_at IS NOT NULL AND reviewed_at IS NOT NULL AND password_hash IS NULL)
        OR (status = 'REJECTED' AND reviewed_at IS NOT NULL AND approved_user_id IS NULL AND password_hash IS NULL)
        OR (status = 'EXPIRED' AND reviewed_at IS NULL AND reviewed_by_user_id IS NULL
            AND approved_user_id IS NULL AND password_hash IS NULL)
    )
);

CREATE UNIQUE INDEX ux_user_registration_requests_pending_email
    ON user_registration_requests(recipient_email)
    WHERE status IN ('PENDING_EMAIL', 'PENDING_APPROVAL');

CREATE INDEX idx_user_registration_requests_status_created
    ON user_registration_requests(status, created_at DESC, id DESC);

CREATE INDEX idx_user_registration_requests_expiry
    ON user_registration_requests(expires_at, id)
    WHERE status IN ('PENDING_EMAIL', 'PENDING_APPROVAL');
