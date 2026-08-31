-- Keep the time-window scans covering so recommendation playback counts do not
-- read every user/session row from the base tables.
CREATE INDEX idx_user_item_state_recommendation_last_played
    ON user_item_state(last_played_at, item_id, user_id)
    WHERE last_played_at IS NOT NULL;

CREATE INDEX idx_playback_sessions_recommendation_last_event
    ON playback_sessions(last_event_at, item_id, user_id);

-- The aggregate only needs rows that currently represent a favorite.
CREATE INDEX idx_user_item_state_recommendation_favorites
    ON user_item_state(item_id)
    WHERE is_favorite = 1;

-- Include the selected image id so the four recommendation image lookups are
-- covered by the index instead of performing a table lookup per item.
CREATE INDEX idx_item_images_recommendation_lookup
    ON item_images(item_id, image_type, image_index, id);
