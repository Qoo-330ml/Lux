ALTER TABLE user_item_state
ADD COLUMN hide_from_resume BIGINT NOT NULL DEFAULT 0 CHECK (hide_from_resume IN (0, 1));
