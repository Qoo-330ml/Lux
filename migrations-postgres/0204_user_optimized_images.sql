ALTER TABLE user_account_preferences
    ADD COLUMN optimized_images BIGINT NOT NULL DEFAULT 0
        CHECK (optimized_images IN (0, 1));
