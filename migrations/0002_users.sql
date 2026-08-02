CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    username_normalized TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    is_disabled INTEGER NOT NULL DEFAULT 0 CHECK (is_disabled IN (0, 1)),
    is_admin INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
    can_manage_server INTEGER NOT NULL DEFAULT 0 CHECK (can_manage_server IN (0, 1)),
    can_remote_access INTEGER NOT NULL DEFAULT 0 CHECK (can_remote_access IN (0, 1)),
    can_download INTEGER NOT NULL DEFAULT 0 CHECK (can_download IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    last_login_at INTEGER
);
