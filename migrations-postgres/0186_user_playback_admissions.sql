CREATE TABLE playback_admissions (
    user_id TEXT NOT NULL,
    play_session_id TEXT NOT NULL,
    state TEXT NOT NULL
        CONSTRAINT chk_playback_admissions_state
        CHECK (state IN ('ACTIVE', 'RELEASED')),
    admitted_at BIGINT NOT NULL,
    last_seen_at BIGINT NOT NULL,
    CONSTRAINT pk_playback_admissions PRIMARY KEY (user_id, play_session_id),
    CONSTRAINT fk_playback_admissions_user
        FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    CONSTRAINT chk_playback_admissions_timestamps
        CHECK (admitted_at >= 0 AND last_seen_at >= admitted_at)
);

CREATE INDEX idx_playback_admissions_active
ON playback_admissions(user_id, state, last_seen_at);
