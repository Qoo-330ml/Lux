CREATE TABLE emby_direct_playback_tickets (
    user_id TEXT NOT NULL,
    play_session_id TEXT NOT NULL
        CHECK (length(trim(play_session_id)) BETWEEN 1 AND 128),
    authorization_device_id TEXT
        CHECK (authorization_device_id IS NULL OR
               length(trim(authorization_device_id)) BETWEEN 1 AND 256),
    authorization_client_name TEXT
        CHECK (authorization_client_name IS NULL OR
               length(trim(authorization_client_name)) BETWEEN 1 AND 256),
    expires_at BIGINT NOT NULL,
    revoked_at BIGINT,
    created_at BIGINT NOT NULL,
    CONSTRAINT pk_emby_direct_playback_tickets
        PRIMARY KEY (user_id, play_session_id),
    CONSTRAINT fk_emby_direct_playback_tickets_user
        FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    CONSTRAINT chk_emby_direct_playback_tickets_timestamps
        CHECK (created_at >= 0 AND expires_at >= created_at AND
               (revoked_at IS NULL OR
                (revoked_at >= created_at AND revoked_at <= expires_at)))
);

CREATE INDEX idx_emby_direct_playback_tickets_expiry
    ON emby_direct_playback_tickets(expires_at);
