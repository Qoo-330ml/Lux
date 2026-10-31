-- Derived "continue watching / next episode" pointer per user and series. Rebuildable from
-- user_item_state + hierarchy; maintained inside the same transaction as progress writes.
CREATE TABLE user_series_next (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    series_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    next_item_id TEXT NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    position_ticks INTEGER NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    played INTEGER NOT NULL DEFAULT 0 CHECK (played IN (0, 1)),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (user_id, series_id)
);

CREATE INDEX idx_user_series_next_user_recent
    ON user_series_next(user_id, updated_at DESC, series_id);
