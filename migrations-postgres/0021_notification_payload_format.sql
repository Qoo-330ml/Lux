ALTER TABLE notification_destinations
    ADD COLUMN payload_format TEXT NOT NULL DEFAULT 'LUX'
    CHECK (payload_format IN ('LUX', 'EMBY'));
