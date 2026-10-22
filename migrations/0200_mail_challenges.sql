CREATE TABLE mail_challenges (
    id TEXT PRIMARY KEY NOT NULL,
    challenge_kind TEXT NOT NULL
        CHECK (challenge_kind IN ('INVITATION', 'EMAIL_VERIFICATION')),
    recipient_email TEXT NOT NULL
        CHECK (length(trim(recipient_email)) BETWEEN 3 AND 320),
    reference_id TEXT NOT NULL
        CHECK (length(trim(reference_id)) BETWEEN 1 AND 128),
    token_hash BLOB NOT NULL
        CHECK (typeof(token_hash) = 'blob' AND length(token_hash) = 32),
    expires_at INTEGER NOT NULL CHECK (expires_at >= 0),
    consumed_at INTEGER,
    revoked_at INTEGER,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()) CHECK (created_at >= 0),
    CHECK (expires_at > created_at),
    CHECK (consumed_at IS NULL OR consumed_at >= created_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at),
    CHECK (consumed_at IS NULL OR revoked_at IS NULL)
);

CREATE INDEX idx_mail_challenges_reference
    ON mail_challenges(challenge_kind, reference_id, created_at DESC);

CREATE INDEX idx_mail_challenges_expiry
    ON mail_challenges(expires_at, consumed_at, revoked_at);
