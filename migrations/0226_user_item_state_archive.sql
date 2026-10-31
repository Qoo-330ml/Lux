-- Watch state that outlives the media item it was recorded against.
--
-- `user_item_state` hangs off `media_items` with ON DELETE CASCADE, so any hard delete of an
-- item (expired soft delete, library removal) took the user's position, played flag, favourite
-- flag and play count with it. When the same title came back -- typically in another library,
-- or in the same library under a new path -- it started from zero even though the content is
-- identical and the user had already watched it.
--
-- The archive keeps one row per (user, item type, provider identity), which is the identity
-- `media_item_provider_ids` already indexes and `merge_suppressions` already keys on. Hard-delete
-- paths copy the state here before the cascade fires, and the background reconciler reattaches it
-- to whichever live item next carries that identity, then deletes the row. Rows therefore only
-- exist for titles that are currently absent, so the table stays bounded by "titles the user has
-- watched and then removed", not by library size.
--
-- Items without a provider identity (unresolved, or local-only matches) are deliberately not
-- archived: there is no identity to reattach them by, and the purge keeps pinning those items
-- instead of dropping the state.
CREATE TABLE user_item_state_archive (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_type TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    position_ticks INTEGER NOT NULL DEFAULT 0 CHECK (position_ticks >= 0),
    is_played INTEGER NOT NULL DEFAULT 0 CHECK (is_played IN (0, 1)),
    is_favorite INTEGER NOT NULL DEFAULT 0 CHECK (is_favorite IN (0, 1)),
    play_count INTEGER NOT NULL DEFAULT 0 CHECK (play_count >= 0),
    last_played_at INTEGER,
    detached_at INTEGER NOT NULL DEFAULT (unixepoch()),
    -- The item the state came from. Kept for operator support, never read by the reconciler.
    source_item_id TEXT NOT NULL,
    PRIMARY KEY (user_id, item_type, provider, provider_id)
);

-- Both the reconciler and manual inspection look rows up by identity, never by user.
CREATE INDEX idx_user_item_state_archive_identity
    ON user_item_state_archive(item_type, provider, provider_id);
