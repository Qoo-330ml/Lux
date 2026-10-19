CREATE TABLE user_access_blacklist_rules (
    id TEXT PRIMARY KEY,
    user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    rule_type TEXT NOT NULL
        CHECK (rule_type IN ('DEVICE', 'CLIENT', 'IP_CIDR')),
    match_value TEXT NOT NULL
        CHECK (length(trim(match_value)) BETWEEN 1 AND 256),
    is_enabled INTEGER NOT NULL DEFAULT 1
        CHECK (is_enabled IN (0, 1)),
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE UNIQUE INDEX uq_user_access_blacklist_global_value
    ON user_access_blacklist_rules(rule_type, match_value)
    WHERE user_id IS NULL;

CREATE UNIQUE INDEX uq_user_access_blacklist_user_value
    ON user_access_blacklist_rules(user_id, rule_type, match_value)
    WHERE user_id IS NOT NULL;

CREATE INDEX idx_user_access_blacklist_global_active
    ON user_access_blacklist_rules(rule_type, match_value)
    WHERE user_id IS NULL AND is_enabled = 1;

CREATE INDEX idx_user_access_blacklist_user_active
    ON user_access_blacklist_rules(user_id, rule_type, match_value)
    WHERE user_id IS NOT NULL AND is_enabled = 1;
