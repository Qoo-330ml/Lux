CREATE TABLE user_devices (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL CHECK (length(trim(device_id)) > 0),
    device_name TEXT,
    device_type TEXT,
    client_name TEXT NOT NULL,
    client_version TEXT,
    first_seen_at BIGINT NOT NULL,
    last_seen_at BIGINT NOT NULL,
    last_remote_ip TEXT,
    PRIMARY KEY (user_id, device_id)
);

CREATE INDEX idx_user_devices_user_activity
    ON user_devices(user_id, last_seen_at DESC, device_id);

WITH ranked_access_tokens AS (
    SELECT
        user_id,
        device_id,
        device_name,
        device_type,
        client_name,
        client_version,
        MIN(created_at) OVER (PARTITION BY user_id, device_id) AS first_seen_at,
        MAX(COALESCE(last_seen_at, created_at))
            OVER (PARTITION BY user_id, device_id) AS last_seen_at,
        ROW_NUMBER() OVER (
            PARTITION BY user_id, device_id
            ORDER BY COALESCE(last_seen_at, created_at) DESC, created_at DESC, id DESC
        ) AS latest_observation
    FROM access_tokens
    WHERE length(trim(device_id)) > 0
)
INSERT INTO user_devices (
    user_id,
    device_id,
    device_name,
    device_type,
    client_name,
    client_version,
    first_seen_at,
    last_seen_at,
    last_remote_ip
)
SELECT
    user_id,
    device_id,
    device_name,
    device_type,
    client_name,
    client_version,
    first_seen_at,
    last_seen_at,
    NULL
FROM ranked_access_tokens
WHERE latest_observation = 1;
