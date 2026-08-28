CREATE INDEX IF NOT EXISTS idx_user_item_state_favorites
ON user_item_state(user_id, is_favorite, item_id);
