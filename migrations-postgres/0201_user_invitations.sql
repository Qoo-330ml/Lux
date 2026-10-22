CREATE TABLE user_invitations (
    id TEXT PRIMARY KEY NOT NULL
        CHECK (length(btrim(id)) BETWEEN 1 AND 128),
    recipient_email TEXT NOT NULL
        CHECK (
            length(btrim(recipient_email)) BETWEEN 3 AND 254
            AND recipient_email = lower(recipient_email)
        ),
    created_by_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    accepted_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING', 'ACCEPTED', 'REVOKED', 'EXPIRED')),
    preset_id TEXT NOT NULL CHECK (length(btrim(preset_id)) BETWEEN 1 AND 128),
    preset_name TEXT NOT NULL CHECK (length(btrim(preset_name)) BETWEEN 1 AND 80),
    preset_version BIGINT NOT NULL CHECK (preset_version >= 1),
    permission_snapshot_json TEXT NOT NULL
        CHECK (
            octet_length(permission_snapshot_json) <= 32768
            AND jsonb_typeof(permission_snapshot_json::jsonb) = 'object'
        ),
    expires_at BIGINT NOT NULL CHECK (expires_at >= 0),
    accepted_at BIGINT,
    revoked_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()) CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch()) CHECK (updated_at >= created_at),
    CHECK (expires_at > created_at),
    CHECK (accepted_at IS NULL OR accepted_at >= created_at),
    CHECK (revoked_at IS NULL OR revoked_at >= created_at),
    CHECK (
        (status = 'PENDING' AND accepted_user_id IS NULL AND accepted_at IS NULL AND revoked_at IS NULL)
        OR (status = 'ACCEPTED' AND accepted_at IS NOT NULL AND revoked_at IS NULL)
        OR (status = 'REVOKED' AND accepted_user_id IS NULL AND accepted_at IS NULL AND revoked_at IS NOT NULL)
        OR (status = 'EXPIRED' AND accepted_user_id IS NULL AND accepted_at IS NULL AND revoked_at IS NULL)
    )
);

CREATE UNIQUE INDEX ux_user_invitations_pending_email
    ON user_invitations(recipient_email)
    WHERE status = 'PENDING';

CREATE INDEX idx_user_invitations_status_created
    ON user_invitations(status, created_at DESC, id DESC);

CREATE INDEX idx_user_invitations_creator_created
    ON user_invitations(created_by_user_id, created_at DESC, id DESC);
