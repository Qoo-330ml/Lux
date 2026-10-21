-- Image optimization is controlled exclusively by the administrator global switch.
ALTER TABLE user_account_preferences DROP COLUMN optimized_images;
