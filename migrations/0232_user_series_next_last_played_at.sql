-- Timestamp of the episode that produced the pointer (the in-progress episode itself, or the
-- latest completed episode when the pointer advances). Continue-watching orders by this value so
-- the derived read model keeps the authoritative ordering without a second aggregation.
ALTER TABLE user_series_next ADD COLUMN last_played_at INTEGER;
