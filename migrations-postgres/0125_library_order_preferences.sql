CREATE TABLE user_library_order_preferences (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    use_admin_library_order BIGINT NOT NULL DEFAULT 1 CHECK (use_admin_library_order IN (0, 1)),
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM NOW())::BIGINT)
);
