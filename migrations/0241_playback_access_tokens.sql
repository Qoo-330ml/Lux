-- Short-lived, playback-only Emby credentials for Lux Web. `scope` keeps them out of every
-- default token lookup (Lux API, login-session lists); `playback_session_id` lets a playback
-- session revoke the tokens it issued.
ALTER TABLE access_tokens ADD COLUMN scope TEXT NOT NULL DEFAULT 'FULL';
ALTER TABLE access_tokens ADD COLUMN playback_session_id TEXT;

CREATE INDEX access_tokens_playback_session_idx
    ON access_tokens(playback_session_id)
    WHERE playback_session_id IS NOT NULL;
