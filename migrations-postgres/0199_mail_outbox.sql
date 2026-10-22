CREATE TABLE mail_outbox (
    id TEXT PRIMARY KEY NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE
        CHECK (length(btrim(idempotency_key)) BETWEEN 1 AND 256),
    recipient_email TEXT NOT NULL
        CHECK (length(btrim(recipient_email)) BETWEEN 3 AND 320),
    template_kind TEXT NOT NULL
        CHECK (template_kind IN ('INVITATION', 'EMAIL_VERIFICATION')),
    reference_id TEXT NOT NULL
        CHECK (length(btrim(reference_id)) BETWEEN 1 AND 128),
    status TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING', 'SENDING', 'RETRY_WAIT', 'SENT', 'FAILED')),
    attempt_count BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    available_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT)
        CHECK (available_at >= 0),
    lease_expires_at BIGINT,
    last_error_code TEXT
        CHECK (last_error_code IS NULL OR last_error_code IN (
            'SMTP_UNAVAILABLE', 'SETTINGS_UNAVAILABLE',
            'TEMPLATE_UNAVAILABLE', 'REFERENCE_UNAVAILABLE'
        )),
    sent_at BIGINT,
    created_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT)
        CHECK (created_at >= 0),
    updated_at BIGINT NOT NULL DEFAULT (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP)::BIGINT)
        CHECK (updated_at >= created_at),
    CHECK ((status = 'SENDING') = (lease_expires_at IS NOT NULL)),
    CHECK ((status = 'SENT') = (sent_at IS NOT NULL)),
    CHECK ((status IN ('RETRY_WAIT', 'FAILED')) = (last_error_code IS NOT NULL))
);

CREATE INDEX idx_mail_outbox_due
    ON mail_outbox(status, available_at, lease_expires_at, created_at, id);

CREATE INDEX idx_mail_outbox_created
    ON mail_outbox(created_at DESC, id DESC);
