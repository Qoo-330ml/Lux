ALTER TABLE user_item_state
ADD COLUMN last_play_session_id TEXT;

UPDATE user_item_state
SET last_play_session_id = (
    SELECT playback_sessions.play_session_id
    FROM playback_sessions
    WHERE playback_sessions.user_id = user_item_state.user_id
      AND playback_sessions.item_id = user_item_state.item_id
      AND playback_sessions.position_ticks = user_item_state.position_ticks
    ORDER BY playback_sessions.started_at DESC, playback_sessions.id DESC
    LIMIT 1
);
