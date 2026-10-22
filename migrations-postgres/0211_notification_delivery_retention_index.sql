-- Support bounded retention sweeps for delivered and failed webhook rows.
CREATE INDEX idx_notification_deliveries_retention
    ON notification_deliveries(created_at, id)
    WHERE status = 'DELIVERED' OR status = 'FAILED';
