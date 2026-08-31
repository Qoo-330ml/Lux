CREATE TABLE recommendation_rating_cache (
    cache_key TEXT PRIMARY KEY NOT NULL,
    median_rating DOUBLE PRECISION NOT NULL,
    calculated_at BIGINT NOT NULL
);

CREATE TABLE recommendation_item_stats (
    item_id TEXT PRIMARY KEY NOT NULL REFERENCES media_items(id) ON DELETE CASCADE,
    recent_playback_score BIGINT NOT NULL DEFAULT 0 CHECK (recent_playback_score >= 0 AND recent_playback_score <= 50),
    favorite_score BIGINT NOT NULL DEFAULT 0 CHECK (favorite_score >= 0 AND favorite_score <= 50),
    refreshed_batch_key BIGINT NOT NULL
);

CREATE TABLE recommendation_stats_state (
    id BIGINT PRIMARY KEY NOT NULL CHECK (id = 1),
    batch_key BIGINT NOT NULL,
    refreshed_at BIGINT NOT NULL
);

CREATE TABLE recommendation_daily_batches (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    library_scope_key TEXT NOT NULL,
    batch_key BIGINT NOT NULL,
    item_ids_json TEXT NOT NULL,
    created_at BIGINT NOT NULL DEFAULT (unixepoch()),
    PRIMARY KEY (user_id, library_scope_key, batch_key)
);

CREATE INDEX idx_recommendation_daily_batches_user
    ON recommendation_daily_batches(user_id, batch_key);

WITH recent_playback_users AS (
    SELECT ps.item_id, ps.user_id
    FROM playback_sessions ps
    WHERE ps.last_event_at > unixepoch() - 15552000
    UNION
    SELECT us.item_id, us.user_id
    FROM user_item_state us
    WHERE us.last_played_at > unixepoch() - 15552000
),
playback_counts AS (
    SELECT item_id, COUNT(*) AS recent_playback_user_count
    FROM recent_playback_users
    GROUP BY item_id
),
favorite_counts AS (
    SELECT item_id, COUNT(*) AS favorite_user_count
    FROM user_item_state
    WHERE is_favorite = 1
    GROUP BY item_id
)
INSERT INTO recommendation_item_stats (
    item_id, recent_playback_score, favorite_score, refreshed_batch_key
)
SELECT mi.id,
       CASE WHEN COALESCE(pc.recent_playback_user_count, 0) > 50
            THEN 50 ELSE COALESCE(pc.recent_playback_user_count, 0) END,
       CASE WHEN 5 * COALESCE(fc.favorite_user_count, 0) > 50
            THEN 50 ELSE 5 * COALESCE(fc.favorite_user_count, 0) END,
       (unixepoch() - 7200) / 86400
FROM media_items mi
JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
LEFT JOIN playback_counts pc ON pc.item_id = mi.id
LEFT JOIN favorite_counts fc ON fc.item_id = mi.id
WHERE mi.removed_at IS NULL
  AND mi.item_type IN ('MOVIE', 'SERIES');

INSERT INTO recommendation_stats_state (id, batch_key, refreshed_at)
VALUES (1, (unixepoch() - 7200) / 86400, unixepoch());
