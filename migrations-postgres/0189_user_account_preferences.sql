CREATE TABLE user_account_preferences (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    theme TEXT NOT NULL DEFAULT 'dark' CHECK (theme IN ('light', 'dark')),
    accent_color TEXT NOT NULL DEFAULT 'silver'
        CHECK (accent_color IN ('silver', 'berry', 'ocean', 'amber', 'mint')),
    show_media_libraries BIGINT NOT NULL DEFAULT 1
        CHECK (show_media_libraries IN (0, 1)),
    show_continue_watching BIGINT NOT NULL DEFAULT 1
        CHECK (show_continue_watching IN (0, 1)),
    audio_language TEXT NOT NULL DEFAULT '原始音轨',
    subtitle_language TEXT NOT NULL DEFAULT '简体中文',
    auto_play_next_episode BIGINT NOT NULL DEFAULT 1
        CHECK (auto_play_next_episode IN (0, 1)),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at BIGINT NOT NULL DEFAULT (unixepoch())
);
