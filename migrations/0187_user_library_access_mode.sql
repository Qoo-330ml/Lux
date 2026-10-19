ALTER TABLE users ADD COLUMN library_access_mode TEXT NOT NULL DEFAULT 'ALL'
    CHECK (library_access_mode IN ('ALL', 'SELECTED', 'NONE'));

UPDATE users
SET library_access_mode = 'SELECTED'
WHERE EXISTS (
    SELECT 1
    FROM user_library_access
    WHERE user_library_access.user_id = users.id
      AND user_library_access.can_view = 1
);
