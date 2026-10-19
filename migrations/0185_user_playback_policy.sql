ALTER TABLE users ADD COLUMN can_play INTEGER NOT NULL DEFAULT 1
    CHECK (can_play IN (0, 1));
ALTER TABLE users ADD COLUMN max_simultaneous_playbacks INTEGER NOT NULL DEFAULT 0
    CHECK (max_simultaneous_playbacks >= 0);
ALTER TABLE users ADD COLUMN allow_remux INTEGER NOT NULL DEFAULT 1
    CHECK (allow_remux IN (0, 1));
ALTER TABLE users ADD COLUMN allow_audio_transcode INTEGER NOT NULL DEFAULT 1
    CHECK (allow_audio_transcode IN (0, 1));
ALTER TABLE users ADD COLUMN allow_video_transcode INTEGER NOT NULL DEFAULT 1
    CHECK (allow_video_transcode IN (0, 1));
