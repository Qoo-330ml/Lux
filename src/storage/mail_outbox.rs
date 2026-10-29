//! Mail outbox queue: append, claim, acknowledge, defer, fail and page queued mail.
//!
//! Split out of repository.rs by touch domain (W6); the queue contract and behaviour are
//! unchanged.

use super::*;

// LUX-783 wires the worker and LUX-784 wires the admin queue API to this storage contract.
#[allow(dead_code)]
impl Database {
    pub(crate) async fn enqueue_mail_outbox(
        &self,
        message: NewMailOutboxMessage<'_>,
        now: i64,
    ) -> Result<StoredMailOutboxMessage, StorageError> {
        if now < 0
            || message.available_at < 0
            || message.idempotency_key.trim() != message.idempotency_key
            || !(1..=256).contains(&message.idempotency_key.len())
            || message.idempotency_key.chars().any(char::is_control)
            || message.recipient_email.trim() != message.recipient_email
            || !(3..=320).contains(&message.recipient_email.len())
            || message.recipient_email.chars().any(char::is_control)
            || message.reference_id.trim() != message.reference_id
            || !(1..=128).contains(&message.reference_id.len())
            || message.reference_id.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict(
                "invalid mail outbox message".to_owned(),
            ));
        }

        let id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO mail_outbox (
                 id, idempotency_key, recipient_email, template_kind, reference_id,
                 status, attempt_count, available_at, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 'PENDING', 0, ?, ?, ?)
             ON CONFLICT (idempotency_key) DO NOTHING",
        )
        .bind(&id)
        .bind(message.idempotency_key)
        .bind(message.recipient_email)
        .bind(message.template_kind.as_str())
        .bind(message.reference_id)
        .bind(message.available_at)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(self.sqlx_err())?;

        let record = self
            .query_as::<MailOutboxRecord>(
                "SELECT id, idempotency_key, recipient_email, template_kind, reference_id,
                        status, attempt_count, available_at, lease_expires_at,
                        last_error_code, sent_at, created_at, updated_at
                 FROM mail_outbox WHERE idempotency_key = ?",
            )
            .bind(message.idempotency_key)
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        if record.recipient_email != message.recipient_email
            || record.template_kind != message.template_kind.as_str()
            || record.reference_id != message.reference_id
        {
            return Err(StorageError::Conflict(
                "mail outbox idempotency key already used".to_owned(),
            ));
        }
        record.try_into()
    }

    pub(crate) async fn claim_mail_outbox(
        &self,
        now: i64,
        lease_seconds: i64,
    ) -> Result<Option<StoredMailOutboxMessage>, StorageError> {
        if now < 0 || !(1..=900).contains(&lease_seconds) {
            return Err(StorageError::Conflict(
                "invalid mail outbox lease".to_owned(),
            ));
        }
        let lease_expires_at = now.saturating_add(lease_seconds);
        let statement = match self.backend {
            DatabaseBackend::Sqlite => {
                "UPDATE mail_outbox
                 SET status = 'SENDING', attempt_count = attempt_count + 1,
                     lease_expires_at = ?, last_error_code = NULL,
                     sent_at = NULL, updated_at = ?
                 WHERE id = (
                     SELECT id FROM mail_outbox
                     WHERE (status IN ('PENDING', 'RETRY_WAIT') AND available_at <= ?)
                        OR (status = 'SENDING' AND lease_expires_at <= ?)
                     ORDER BY available_at, created_at, id
                     LIMIT 1
                 )
                 RETURNING id, idempotency_key, recipient_email, template_kind,
                           reference_id, status, attempt_count, available_at,
                           lease_expires_at, last_error_code, sent_at, created_at, updated_at"
            }
            DatabaseBackend::Postgres => {
                "WITH candidate AS (
                     SELECT id FROM mail_outbox
                     WHERE (status IN ('PENDING', 'RETRY_WAIT') AND available_at <= ?)
                        OR (status = 'SENDING' AND lease_expires_at <= ?)
                     ORDER BY available_at, created_at, id
                     LIMIT 1 FOR UPDATE SKIP LOCKED
                 )
                 UPDATE mail_outbox AS outbox
                 SET status = 'SENDING', attempt_count = outbox.attempt_count + 1,
                     lease_expires_at = ?, last_error_code = NULL,
                     sent_at = NULL, updated_at = ?
                 FROM candidate
                 WHERE outbox.id = candidate.id
                 RETURNING outbox.id, outbox.idempotency_key, outbox.recipient_email,
                           outbox.template_kind, outbox.reference_id, outbox.status,
                           outbox.attempt_count, outbox.available_at,
                           outbox.lease_expires_at, outbox.last_error_code,
                           outbox.sent_at, outbox.created_at, outbox.updated_at"
            }
        };
        let mut query = self.query_as::<MailOutboxRecord>(statement);
        if self.backend == DatabaseBackend::Postgres {
            query = query.bind(now).bind(now);
        }
        query = query.bind(lease_expires_at).bind(now);
        if self.backend == DatabaseBackend::Sqlite {
            query = query.bind(now).bind(now);
        }
        query
            .fetch_optional(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .map(TryInto::try_into)
            .transpose()
    }

    pub(crate) async fn mark_mail_outbox_sent(
        &self,
        id: &str,
        attempt_count: i64,
        now: i64,
    ) -> Result<bool, StorageError> {
        if attempt_count <= 0 || now < 0 {
            return Err(StorageError::Conflict(
                "invalid mail outbox completion".to_owned(),
            ));
        }
        let result = self
            .query(
                "UPDATE mail_outbox
                 SET status = 'SENT', lease_expires_at = NULL, last_error_code = NULL,
                     sent_at = ?, updated_at = ?
                 WHERE id = ? AND status = 'SENDING' AND attempt_count = ?",
            )
            .bind(now)
            .bind(now)
            .bind(id)
            .bind(attempt_count)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn defer_mail_outbox(
        &self,
        id: &str,
        attempt_count: i64,
        available_at: i64,
        error_code: MailDeliveryErrorCode,
        now: i64,
    ) -> Result<bool, StorageError> {
        if attempt_count <= 0 || now < 0 || available_at < now {
            return Err(StorageError::Conflict("invalid mail retry time".to_owned()));
        }
        let result = self
            .query(
                "UPDATE mail_outbox
                 SET status = 'RETRY_WAIT', available_at = ?, lease_expires_at = NULL,
                     last_error_code = ?, sent_at = NULL, updated_at = ?
                 WHERE id = ? AND status = 'SENDING' AND attempt_count = ?",
            )
            .bind(available_at)
            .bind(error_code.as_str())
            .bind(now)
            .bind(id)
            .bind(attempt_count)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn fail_mail_outbox(
        &self,
        id: &str,
        attempt_count: i64,
        error_code: MailDeliveryErrorCode,
        now: i64,
    ) -> Result<bool, StorageError> {
        if attempt_count <= 0 || now < 0 {
            return Err(StorageError::Conflict(
                "invalid mail outbox failure".to_owned(),
            ));
        }
        let result = self
            .query(
                "UPDATE mail_outbox
                 SET status = 'FAILED', lease_expires_at = NULL, last_error_code = ?,
                     sent_at = NULL, updated_at = ?
                 WHERE id = ? AND status = 'SENDING' AND attempt_count = ?",
            )
            .bind(error_code.as_str())
            .bind(now)
            .bind(id)
            .bind(attempt_count)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn retry_failed_mail_outbox(
        &self,
        id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        if now < 0 {
            return Err(StorageError::Conflict(
                "invalid mail outbox retry time".to_owned(),
            ));
        }
        let result = self
            .query(
                "UPDATE mail_outbox
                 SET status = 'PENDING', attempt_count = 0, available_at = ?,
                     last_error_code = NULL, sent_at = NULL, updated_at = ?
                 WHERE id = ? AND status = 'FAILED'",
            )
            .bind(now)
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn list_mail_outbox_page(
        &self,
        status: Option<MailOutboxStatus>,
        page_size: i64,
        offset: i64,
    ) -> Result<MailOutboxPage, StorageError> {
        let page_size = page_size.clamp(1, 100);
        let offset = offset.max(0);
        let total_count = if let Some(status) = status {
            self.query_scalar::<i64>("SELECT COUNT(*) FROM mail_outbox WHERE status = ?")
                .bind(status.as_str())
                .fetch_one(&self.pool)
                .await
        } else {
            self.query_scalar::<i64>("SELECT COUNT(*) FROM mail_outbox")
                .fetch_one(&self.pool)
                .await
        }
        .map_err(self.sqlx_err())?;

        let records = if let Some(status) = status {
            self.query_as::<MailOutboxRecord>(
                "SELECT id, idempotency_key, recipient_email, template_kind, reference_id,
                        status, attempt_count, available_at, lease_expires_at,
                        last_error_code, sent_at, created_at, updated_at
                 FROM mail_outbox WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status.as_str())
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query_as::<MailOutboxRecord>(
                "SELECT id, idempotency_key, recipient_email, template_kind, reference_id,
                        status, attempt_count, available_at, lease_expires_at,
                        last_error_code, sent_at, created_at, updated_at
                 FROM mail_outbox ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(self.sqlx_err())?;
        let messages = records
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<StoredMailOutboxMessage>, StorageError>>()?;
        Ok(MailOutboxPage {
            messages,
            total_count,
        })
    }
}
