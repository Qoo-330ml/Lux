-- Allow the daily recommendation materialization to skip recomputation when
-- playback, favorites and media items have not changed since the last run.
ALTER TABLE recommendation_stats_state
    ADD COLUMN source_fingerprint TEXT;
