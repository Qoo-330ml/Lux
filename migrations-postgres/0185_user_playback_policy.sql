ALTER TABLE users
    ADD COLUMN can_play BIGINT NOT NULL DEFAULT 1 CHECK (can_play IN (0, 1)),
    ADD COLUMN max_simultaneous_playbacks BIGINT NOT NULL DEFAULT 0
        CHECK (max_simultaneous_playbacks >= 0),
    ADD COLUMN allow_remux BIGINT NOT NULL DEFAULT 1 CHECK (allow_remux IN (0, 1)),
    ADD COLUMN allow_audio_transcode BIGINT NOT NULL DEFAULT 1
        CHECK (allow_audio_transcode IN (0, 1)),
    ADD COLUMN allow_video_transcode BIGINT NOT NULL DEFAULT 1
        CHECK (allow_video_transcode IN (0, 1));
