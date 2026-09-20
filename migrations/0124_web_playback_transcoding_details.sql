ALTER TABLE web_playback_sessions ADD COLUMN video_codec TEXT;
ALTER TABLE web_playback_sessions ADD COLUMN audio_codec TEXT;
ALTER TABLE web_playback_sessions ADD COLUMN video_bitrate BIGINT;
ALTER TABLE web_playback_sessions ADD COLUMN audio_bitrate BIGINT;
ALTER TABLE web_playback_sessions ADD COLUMN transcoding_container TEXT;
