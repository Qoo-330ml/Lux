ALTER TABLE notification_destinations
    ADD COLUMN provider_plugin_id TEXT NOT NULL DEFAULT 'builtin.webhook';

ALTER TABLE notification_destinations
    ADD COLUMN provider_config_json TEXT NOT NULL DEFAULT '{}';

CREATE INDEX idx_notification_destinations_provider
    ON notification_destinations(provider_plugin_id, enabled, id);
