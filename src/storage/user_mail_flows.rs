//! Invitations, email-change challenges and registration mail flows.
//!
//! Split out of repository.rs by touch domain (W6); the storage contract and behaviour are
//! unchanged.

use super::*;

impl Database {
    #[cfg(test)]
    pub(crate) async fn create_mail_challenge(
        &self,
        challenge: NewMailChallenge<'_>,
        now: i64,
    ) -> Result<StoredMailChallenge, StorageError> {
        if now < 0
            || challenge.expires_at <= now
            || challenge.id.trim() != challenge.id
            || !(1..=128).contains(&challenge.id.len())
            || challenge.id.chars().any(char::is_control)
            || challenge.token_hash.len() != 32
            || challenge.recipient_email.trim() != challenge.recipient_email
            || !(3..=320).contains(&challenge.recipient_email.len())
            || challenge.recipient_email.chars().any(char::is_control)
            || challenge.reference_id.trim() != challenge.reference_id
            || !(1..=128).contains(&challenge.reference_id.len())
            || challenge.reference_id.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict("invalid mail challenge".to_owned()));
        }

        self.query_as::<MailChallengeRecord>(
            "INSERT INTO mail_challenges (
                 id, challenge_kind, recipient_email, reference_id, token_hash,
                 expires_at, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id, challenge_kind, recipient_email, reference_id, token_hash,
                       expires_at, consumed_at, revoked_at, created_at",
        )
        .bind(challenge.id)
        .bind(challenge.kind.as_str())
        .bind(challenge.recipient_email)
        .bind(challenge.reference_id)
        .bind(challenge.token_hash)
        .bind(challenge.expires_at)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .try_into()
    }

    pub(crate) async fn create_user_email_change_with_mail(
        &self,
        user_id: &str,
        challenge: NewMailChallenge<'_>,
        message: NewMailOutboxMessage<'_>,
        expected_pending_challenge_id: Option<&str>,
        now: i64,
    ) -> Result<UserEmailChangeCreateResult, StorageError> {
        if now < 0
            || challenge.kind != MailChallengeKind::EmailVerification
            || challenge.reference_id != user_id
            || challenge.expires_at <= now
            || challenge.id.trim() != challenge.id
            || !(1..=128).contains(&challenge.id.len())
            || challenge.id.chars().any(char::is_control)
            || challenge.token_hash.len() != 32
            || challenge.recipient_email.trim() != challenge.recipient_email
            || challenge.recipient_email != challenge.recipient_email.to_lowercase()
            || !(3..=254).contains(&challenge.recipient_email.len())
            || challenge.recipient_email.chars().any(char::is_control)
            || user_id.trim() != user_id
            || !(1..=128).contains(&user_id.len())
            || user_id.chars().any(char::is_control)
            || expected_pending_challenge_id.is_some_and(|id| {
                id.trim() != id
                    || !(1..=128).contains(&id.len())
                    || id.chars().any(char::is_control)
            })
            || message.idempotency_key.trim() != message.idempotency_key
            || !(1..=256).contains(&message.idempotency_key.len())
            || message.idempotency_key.chars().any(char::is_control)
            || message.recipient_email != challenge.recipient_email
            || message.template_kind != MailOutboxTemplateKind::EmailVerification
            || message.reference_id != challenge.id
            || message.available_at != now
        {
            return Err(StorageError::Conflict(
                "invalid user email change mail request".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let active_user_statement = format!(
            "SELECT id FROM users WHERE id = ? AND is_disabled = 0
             AND deleted_at IS NULL{lock_clause}"
        );
        let active_user_id: Option<String> = self
            .query_scalar(sqlx::AssertSqlSafe(active_user_statement))
            .bind(user_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if active_user_id.is_none() {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserEmailChangeCreateResult::NotAcceptable);
        }

        if let Some(expected_challenge_id) = expected_pending_challenge_id {
            let current_pending_challenge_id: Option<String> = self
                .query_scalar(
                    "SELECT id FROM mail_challenges
                     WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
                       AND expires_at > ? AND created_at <= ?
                       AND consumed_at IS NULL AND revoked_at IS NULL
                     ORDER BY created_at DESC, id DESC LIMIT 1",
                )
                .bind(user_id)
                .bind(now)
                .bind(now)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            if current_pending_challenge_id.as_deref() != Some(expected_challenge_id) {
                transaction.commit().await.map_err(self.sqlx_err())?;
                return Ok(UserEmailChangeCreateResult::NotAcceptable);
            }
        }

        self.query(
            "INSERT INTO mail_challenges (
                 id, challenge_kind, recipient_email, reference_id, token_hash,
                 expires_at, created_at
             ) VALUES (?, 'EMAIL_VERIFICATION', ?, ?, ?, ?, ?)",
        )
        .bind(challenge.id)
        .bind(challenge.recipient_email)
        .bind(user_id)
        .bind(challenge.token_hash)
        .bind(challenge.expires_at)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        let outbox_id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO mail_outbox (
                 id, idempotency_key, recipient_email, template_kind, reference_id,
                 status, attempt_count, available_at, created_at, updated_at
             ) VALUES (?, ?, ?, 'EMAIL_VERIFICATION', ?, 'PENDING', 0, ?, ?, ?)",
        )
        .bind(&outbox_id)
        .bind(message.idempotency_key)
        .bind(message.recipient_email)
        .bind(message.reference_id)
        .bind(message.available_at)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        self.query(
            "UPDATE mail_challenges SET revoked_at = ?
             WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
               AND id <> ? AND consumed_at IS NULL AND revoked_at IS NULL
               AND created_at <= ?",
        )
        .bind(now)
        .bind(user_id)
        .bind(challenge.id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE mail_outbox
             SET status = 'FAILED', lease_expires_at = NULL,
                 last_error_code = 'REFERENCE_UNAVAILABLE', sent_at = NULL,
                 updated_at = ?
             WHERE reference_id IN (
                 SELECT id FROM mail_challenges
                 WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
                   AND id <> ?
             ) AND status IN ('PENDING', 'RETRY_WAIT', 'SENDING')",
        )
        .bind(now)
        .bind(user_id)
        .bind(challenge.id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserEmailChangeCreateResult::Created)
    }

    pub(crate) async fn clear_user_email_change(
        &self,
        user_id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        if now < 0
            || user_id.trim() != user_id
            || !(1..=128).contains(&user_id.len())
            || user_id.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict(
                "invalid user email change clearing request".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let active_user_statement = format!(
            "SELECT id FROM users WHERE id = ? AND is_disabled = 0
             AND deleted_at IS NULL{lock_clause}"
        );
        let active_user_id: Option<String> = self
            .query_scalar(sqlx::AssertSqlSafe(active_user_statement))
            .bind(user_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if active_user_id.is_none() {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(false);
        }

        self.query(
            "UPDATE users SET email = NULL, email_verified_at = NULL, updated_at = ?
             WHERE id = ? AND is_disabled = 0 AND deleted_at IS NULL",
        )
        .bind(now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE mail_challenges SET revoked_at = ?
             WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
               AND consumed_at IS NULL AND revoked_at IS NULL AND created_at <= ?",
        )
        .bind(now)
        .bind(user_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE mail_outbox
             SET status = 'FAILED', lease_expires_at = NULL,
                 last_error_code = 'REFERENCE_UNAVAILABLE', sent_at = NULL,
                 updated_at = ?
             WHERE reference_id IN (
                 SELECT id FROM mail_challenges
                 WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
             ) AND status IN ('PENDING', 'RETRY_WAIT', 'SENDING')",
        )
        .bind(now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(true)
    }

    pub(crate) async fn create_user_invitation_with_mail(
        &self,
        invitation: NewUserInvitation<'_>,
        challenge: NewMailChallenge<'_>,
        message: NewMailOutboxMessage<'_>,
        now: i64,
    ) -> Result<UserInvitationCreateResult, StorageError> {
        let snapshot =
            serde_json::from_str::<serde_json::Value>(invitation.permission_snapshot_json)
                .map_err(|_| {
                    StorageError::Conflict("invalid invitation permission snapshot".to_owned())
                })?;
        if now < 0
            || invitation.id.trim() != invitation.id
            || !(1..=128).contains(&invitation.id.len())
            || invitation.id.chars().any(char::is_control)
            || invitation.recipient_email.trim() != invitation.recipient_email
            || invitation.recipient_email != invitation.recipient_email.to_lowercase()
            || !(3..=254).contains(&invitation.recipient_email.len())
            || invitation.recipient_email.chars().any(char::is_control)
            || invitation
                .created_by_user_id
                .is_some_and(|id| id.trim() != id || !(1..=128).contains(&id.len()))
            || invitation.preset_id.trim() != invitation.preset_id
            || !(1..=128).contains(&invitation.preset_id.len())
            || invitation.preset_name.trim() != invitation.preset_name
            || !(1..=80).contains(&invitation.preset_name.chars().count())
            || invitation.preset_version < 1
            || invitation.permission_snapshot_json.len() > 32_768
            || !snapshot.is_object()
            || invitation.expires_at <= now
            || challenge.id.trim() != challenge.id
            || !(1..=128).contains(&challenge.id.len())
            || challenge.id.chars().any(char::is_control)
            || challenge.kind != MailChallengeKind::Invitation
            || challenge.recipient_email != invitation.recipient_email
            || challenge.reference_id != invitation.id
            || challenge.token_hash.len() != 32
            || challenge.expires_at != invitation.expires_at
            || message.idempotency_key.trim() != message.idempotency_key
            || !(1..=256).contains(&message.idempotency_key.len())
            || message.idempotency_key.chars().any(char::is_control)
            || message.recipient_email != invitation.recipient_email
            || message.template_kind != MailOutboxTemplateKind::Invitation
            || message.reference_id != challenge.id
            || message.available_at != now
        {
            return Err(StorageError::Conflict(
                "invalid user invitation and mail bundle".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE user_invitations
             SET status = 'EXPIRED', updated_at = ?
             WHERE recipient_email = ? AND status = 'PENDING' AND expires_at <= ?",
        )
        .bind(now)
        .bind(invitation.recipient_email)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        let inserted = self
            .query(
                "INSERT INTO user_invitations (
                     id, recipient_email, created_by_user_id, status, preset_id,
                     preset_name, preset_version, permission_snapshot_json,
                     expires_at, created_at, updated_at
                 ) VALUES (?, ?, ?, 'PENDING', ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT DO NOTHING",
            )
            .bind(invitation.id)
            .bind(invitation.recipient_email)
            .bind(invitation.created_by_user_id)
            .bind(invitation.preset_id)
            .bind(invitation.preset_name)
            .bind(invitation.preset_version)
            .bind(invitation.permission_snapshot_json)
            .bind(invitation.expires_at)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if inserted.rows_affected() != 1 {
            let pending_invitation_exists: Option<String> = self
                .query_scalar(
                    "SELECT id FROM user_invitations
                     WHERE recipient_email = ? AND status = 'PENDING' AND expires_at > ?
                     LIMIT 1",
                )
                .bind(invitation.recipient_email)
                .bind(now)
                .fetch_one(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            transaction.commit().await.map_err(self.sqlx_err())?;
            return if pending_invitation_exists.is_some() {
                Ok(UserInvitationCreateResult::AlreadyPending)
            } else {
                Err(StorageError::Conflict(
                    "user invitation ID already exists".to_owned(),
                ))
            };
        }

        let recent_email_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM mail_outbox
                 WHERE recipient_email = ? AND template_kind = 'INVITATION'
                   AND created_at > ?",
            )
            .bind(invitation.recipient_email)
            .bind(now.saturating_sub(INVITATION_EMAIL_RATE_WINDOW_SECONDS))
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if recent_email_count >= INVITATION_EMAIL_RATE_LIMIT_PER_HOUR {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(UserInvitationCreateResult::RateLimited);
        }

        self.query(
            "INSERT INTO mail_challenges (
                 id, challenge_kind, recipient_email, reference_id, token_hash,
                 expires_at, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(challenge.id)
        .bind(challenge.kind.as_str())
        .bind(challenge.recipient_email)
        .bind(challenge.reference_id)
        .bind(challenge.token_hash)
        .bind(challenge.expires_at)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        let outbox_id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO mail_outbox (
                 id, idempotency_key, recipient_email, template_kind, reference_id,
                 status, attempt_count, available_at, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 'PENDING', 0, ?, ?, ?)",
        )
        .bind(&outbox_id)
        .bind(message.idempotency_key)
        .bind(message.recipient_email)
        .bind(message.template_kind.as_str())
        .bind(message.reference_id)
        .bind(message.available_at)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserInvitationCreateResult::Created)
    }

    pub(crate) async fn queue_user_invitation_resend(
        &self,
        invitation_id: &str,
        idempotency_key: &str,
        now: i64,
    ) -> Result<UserInvitationResendResult, StorageError> {
        if now < 0
            || invitation_id.trim() != invitation_id
            || !(1..=128).contains(&invitation_id.len())
            || invitation_id.chars().any(char::is_control)
            || idempotency_key.trim() != idempotency_key
            || !(1..=256).contains(&idempotency_key.len())
            || idempotency_key.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict(
                "invalid user invitation resend".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let invitation_statement = format!(
            "SELECT recipient_email, expires_at FROM user_invitations
             WHERE id = ? AND status = 'PENDING' AND expires_at > ?{lock_clause}"
        );
        let invitation: Option<(String, i64)> = self
            .query_as::<(String, i64)>(sqlx::AssertSqlSafe(invitation_statement))
            .bind(invitation_id)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some((recipient_email, invitation_expires_at)) = invitation else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserInvitationResendResult::NotResendable);
        };

        let challenge_statement = format!(
            "SELECT id FROM mail_challenges
             WHERE challenge_kind = 'INVITATION' AND reference_id = ?
               AND recipient_email = ? AND expires_at = ? AND expires_at > ?
               AND created_at <= ? AND consumed_at IS NULL AND revoked_at IS NULL
             ORDER BY created_at DESC, id DESC LIMIT 1{lock_clause}"
        );
        let challenge_id: Option<String> = self
            .query_scalar(sqlx::AssertSqlSafe(challenge_statement))
            .bind(invitation_id)
            .bind(&recipient_email)
            .bind(invitation_expires_at)
            .bind(now)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some(challenge_id) = challenge_id else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserInvitationResendResult::NotResendable);
        };

        let existing_message = self
            .query_as::<MailOutboxRecord>(
                "SELECT id, idempotency_key, recipient_email, template_kind, reference_id,
                        status, attempt_count, available_at, lease_expires_at,
                        last_error_code, sent_at, created_at, updated_at
                 FROM mail_outbox WHERE idempotency_key = ?",
            )
            .bind(idempotency_key)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if let Some(existing_message) = existing_message {
            if existing_message.recipient_email != recipient_email
                || existing_message.template_kind != MailOutboxTemplateKind::Invitation.as_str()
                || existing_message.reference_id != challenge_id
            {
                return Err(StorageError::Conflict(
                    "mail outbox idempotency key already used".to_owned(),
                ));
            }
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserInvitationResendResult::Queued);
        }

        let recent_email_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM mail_outbox
                 WHERE recipient_email = ? AND template_kind = 'INVITATION'
                   AND created_at > ?",
            )
            .bind(&recipient_email)
            .bind(now.saturating_sub(INVITATION_EMAIL_RATE_WINDOW_SECONDS))
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if recent_email_count >= INVITATION_EMAIL_RATE_LIMIT_PER_HOUR {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserInvitationResendResult::RateLimited);
        }

        let outbox_id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO mail_outbox (
                 id, idempotency_key, recipient_email, template_kind, reference_id,
                 status, attempt_count, available_at, created_at, updated_at
             ) VALUES (?, ?, ?, 'INVITATION', ?, 'PENDING', 0, ?, ?, ?)
             ON CONFLICT (idempotency_key) DO NOTHING",
        )
        .bind(&outbox_id)
        .bind(idempotency_key)
        .bind(&recipient_email)
        .bind(&challenge_id)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        let existing_message = self
            .query_as::<MailOutboxRecord>(
                "SELECT id, idempotency_key, recipient_email, template_kind, reference_id,
                        status, attempt_count, available_at, lease_expires_at,
                        last_error_code, sent_at, created_at, updated_at
                 FROM mail_outbox WHERE idempotency_key = ?",
            )
            .bind(idempotency_key)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if existing_message.recipient_email != recipient_email
            || existing_message.template_kind != MailOutboxTemplateKind::Invitation.as_str()
            || existing_message.reference_id != challenge_id
        {
            return Err(StorageError::Conflict(
                "mail outbox idempotency key already used".to_owned(),
            ));
        }

        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserInvitationResendResult::Queued)
    }

    pub(crate) async fn get_user_invitation(
        &self,
        id: &str,
        now: i64,
    ) -> Result<Option<StoredUserInvitation>, StorageError> {
        if now < 0 {
            return Err(StorageError::Conflict(
                "invalid invitation read time".to_owned(),
            ));
        }
        self.query_as::<UserInvitationRecord>(
            "SELECT id, recipient_email, created_by_user_id, accepted_user_id,
                    CASE WHEN status = 'PENDING' AND expires_at <= ?
                         THEN 'EXPIRED' ELSE status END AS status,
                    preset_id, preset_name, preset_version, permission_snapshot_json,
                    expires_at, accepted_at, revoked_at, created_at, updated_at
             FROM user_invitations WHERE id = ?",
        )
        .bind(now)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .map(TryInto::try_into)
        .transpose()
    }

    pub(crate) async fn get_user_invitation_for_challenge_hash(
        &self,
        token_hash: &[u8],
        now: i64,
    ) -> Result<Option<StoredUserInvitationAcceptance>, StorageError> {
        if now < 0 || token_hash.len() != 32 {
            return Err(StorageError::Conflict(
                "invalid invitation challenge lookup".to_owned(),
            ));
        }
        let row = self
            .query_as::<(String, String, String, i64)>(
                "SELECT i.id, c.id, c.recipient_email, c.expires_at
                 FROM mail_challenges c
                 JOIN user_invitations i ON i.id = c.reference_id
                 WHERE c.challenge_kind = 'INVITATION' AND c.token_hash = ?
                   AND c.expires_at > ? AND c.created_at <= ?
                   AND c.consumed_at IS NULL AND c.revoked_at IS NULL
                   AND i.status = 'PENDING' AND i.expires_at > ?
                   AND i.recipient_email = c.recipient_email
                   AND i.expires_at = c.expires_at
                 ORDER BY c.created_at DESC, c.id DESC LIMIT 1",
            )
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .bind(now)
            .fetch_optional(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(row.map(
            |(invitation_id, challenge_id, recipient_email, expires_at)| {
                StoredUserInvitationAcceptance {
                    invitation_id,
                    challenge_id,
                    recipient_email,
                    expires_at,
                }
            },
        ))
    }

    pub(crate) async fn accept_user_invitation(
        &self,
        challenge_id: &str,
        token_hash: &[u8],
        account: NewInvitationUserAccount<'_>,
        now: i64,
    ) -> Result<UserInvitationAcceptResult, StorageError> {
        if now < 0
            || challenge_id.trim() != challenge_id
            || !(1..=128).contains(&challenge_id.len())
            || challenge_id.chars().any(char::is_control)
            || token_hash.len() != 32
            || account.id.trim() != account.id
            || !(1..=128).contains(&account.id.len())
            || account.id.chars().any(char::is_control)
            || account.username_normalized.trim() != account.username_normalized
            || account.username_normalized.is_empty()
            || account.username_normalized.chars().count() > 128
            || account.username_normalized != account.username_normalized.to_lowercase()
            || account.display_name.trim().is_empty()
            || account.display_name.chars().count() > 128
            || account.password_hash.trim().is_empty()
            || account.password_hash.len() > 512
        {
            return Err(StorageError::Conflict(
                "invalid invitation acceptance".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let invitation_statement = format!(
            "SELECT i.id, i.recipient_email, i.permission_snapshot_json FROM user_invitations i
             JOIN mail_challenges c ON c.reference_id = i.id
             WHERE c.id = ? AND c.challenge_kind = 'INVITATION' AND c.token_hash = ?
               AND c.expires_at > ? AND c.created_at <= ?
               AND c.consumed_at IS NULL AND c.revoked_at IS NULL
               AND i.status = 'PENDING' AND i.expires_at > ?
               AND i.recipient_email = c.recipient_email
               AND i.expires_at = c.expires_at{lock_clause}"
        );
        let invitation: Option<(String, String, String)> = self
            .query_as(sqlx::AssertSqlSafe(invitation_statement))
            .bind(challenge_id)
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some((invitation_id, recipient_email, permission_snapshot_json)) = invitation else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserInvitationAcceptResult::NotAcceptable);
        };

        let permission_snapshot: UserInvitationPermissionSnapshot =
            serde_json::from_str(&permission_snapshot_json).map_err(|_| {
                StorageError::Serialization("invitation permission snapshot is invalid".to_owned())
            })?;
        let permissions = permission_snapshot.permissions;
        if !matches!(
            permissions.library_access_mode.as_str(),
            "ALL" | "SELECTED" | "NONE"
        ) || permissions.max_simultaneous_playbacks < 0
        {
            return Err(StorageError::Serialization(
                "invitation permission snapshot is invalid".to_owned(),
            ));
        }

        let username_taken = self
            .query_scalar::<String>("SELECT id FROM users WHERE username_normalized = ? LIMIT 1")
            .bind(account.username_normalized)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .is_some();
        if username_taken {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserInvitationAcceptResult::UsernameTaken);
        }

        let insert_user = self
            .query(
                "INSERT INTO users (
                    id, username_normalized, display_name, password_hash,
                    is_admin, can_manage_server, has_password, email, email_verified_at,
                    can_download, can_play, max_simultaneous_playbacks,
                    allow_remux, allow_audio_transcode, allow_video_transcode,
                    library_access_mode
                 ) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(account.id)
            .bind(account.username_normalized)
            .bind(account.display_name)
            .bind(account.password_hash)
            .bind(database_flag(permission_snapshot.is_admin))
            .bind(database_flag(permission_snapshot.can_manage_server))
            .bind(&recipient_email)
            .bind(now)
            .bind(database_flag(permissions.can_download))
            .bind(database_flag(permissions.can_play))
            .bind(permissions.max_simultaneous_playbacks)
            .bind(database_flag(permissions.allow_remux))
            .bind(database_flag(permissions.allow_audio_transcode))
            .bind(database_flag(permissions.allow_video_transcode))
            .bind(&permissions.library_access_mode)
            .execute(&mut *transaction)
            .await;
        if let Err(source) = insert_user {
            let unique_violation = source
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation());
            if unique_violation {
                transaction.rollback().await.map_err(self.sqlx_err())?;

                let username_taken = self
                    .query_scalar::<String>(
                        "SELECT id FROM users WHERE username_normalized = ? LIMIT 1",
                    )
                    .bind(account.username_normalized)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(self.sqlx_err())?
                    .is_some();
                if username_taken {
                    return Ok(UserInvitationAcceptResult::UsernameTaken);
                }

                let email_already_registered = self
                    .query_scalar::<String>(
                        "SELECT id FROM users
                         WHERE email IS NOT NULL AND email <> ''
                           AND LOWER(email) = LOWER(?)
                         LIMIT 1",
                    )
                    .bind(&recipient_email)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(self.sqlx_err())?
                    .is_some();
                if email_already_registered {
                    return Ok(UserInvitationAcceptResult::EmailAlreadyRegistered);
                }
            }
            return Err(StorageError::Sqlx {
                path: self.path.clone(),
                source,
            });
        }

        if permissions.library_access_mode == "SELECTED" {
            for library_id in &permissions.library_ids {
                self.query(
                    "INSERT INTO user_library_access (user_id, library_id, can_view)
                     VALUES (?, ?, 1)",
                )
                .bind(account.id)
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            }
        }

        let invitation_updated = self
            .query(
                "UPDATE user_invitations
                 SET status = 'ACCEPTED', accepted_user_id = ?, accepted_at = ?, updated_at = ?
                 WHERE id = ? AND status = 'PENDING' AND expires_at > ?",
            )
            .bind(account.id)
            .bind(now)
            .bind(now)
            .bind(&invitation_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        let challenge_consumed = self
            .query(
                "UPDATE mail_challenges SET consumed_at = ?
                 WHERE id = ? AND token_hash = ? AND challenge_kind = 'INVITATION'
                   AND consumed_at IS NULL AND revoked_at IS NULL
                   AND expires_at > ? AND created_at <= ?",
            )
            .bind(now)
            .bind(challenge_id)
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        if invitation_updated != 1 || challenge_consumed != 1 {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(UserInvitationAcceptResult::NotAcceptable);
        }

        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserInvitationAcceptResult::Accepted)
    }

    pub(crate) async fn list_user_invitations_page(
        &self,
        status: Option<UserInvitationStatus>,
        page_size: i64,
        offset: i64,
        now: i64,
    ) -> Result<UserInvitationPage, StorageError> {
        if now < 0 {
            return Err(StorageError::Conflict(
                "invalid invitation page time".to_owned(),
            ));
        }
        let page_size = page_size.clamp(1, 100);
        let offset = offset.max(0);
        let status_filter = if status.is_some() {
            " WHERE status = ?"
        } else {
            ""
        };
        let count_statement = format!(
            "SELECT COUNT(*) FROM (
                 SELECT CASE WHEN status = 'PENDING' AND expires_at <= ?
                             THEN 'EXPIRED' ELSE status END AS status
                 FROM user_invitations
             ) AS invitations{status_filter}"
        );
        let mut count_query = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_statement));
        count_query = count_query.bind(now);
        if let Some(status) = status {
            count_query = count_query.bind(status.as_str());
        }
        let total_count = count_query
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;

        let page_statement = format!(
            "SELECT id, recipient_email, created_by_user_id, accepted_user_id, status,
                    preset_id, preset_name, preset_version, permission_snapshot_json,
                    expires_at, accepted_at, revoked_at, created_at, updated_at
             FROM (
                 SELECT id, recipient_email, created_by_user_id, accepted_user_id,
                        CASE WHEN status = 'PENDING' AND expires_at <= ?
                             THEN 'EXPIRED' ELSE status END AS status,
                        preset_id, preset_name, preset_version, permission_snapshot_json,
                        expires_at, accepted_at, revoked_at, created_at, updated_at
                 FROM user_invitations
             ) AS invitations{status_filter}
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        );
        let mut page_query = self
            .query_as::<UserInvitationRecord>(sqlx::AssertSqlSafe(page_statement))
            .bind(now);
        if let Some(status) = status {
            page_query = page_query.bind(status.as_str());
        }
        let records = page_query
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let entries = records
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<StoredUserInvitation>, StorageError>>()?;
        Ok(UserInvitationPage {
            entries,
            page_size,
            total_count,
        })
    }

    pub(crate) async fn revoke_user_invitation(
        &self,
        id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        if now < 0 || id.trim() != id || !(1..=128).contains(&id.len()) {
            return Err(StorageError::Conflict(
                "invalid invitation revocation".to_owned(),
            ));
        }
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE user_invitations
             SET status = 'EXPIRED', updated_at = ?
             WHERE id = ? AND status = 'PENDING' AND expires_at <= ?",
        )
        .bind(now)
        .bind(id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        let revoked = self
            .query(
                "UPDATE user_invitations
                 SET status = 'REVOKED', revoked_at = ?, updated_at = ?
                 WHERE id = ? AND status = 'PENDING' AND expires_at > ?",
            )
            .bind(now)
            .bind(now)
            .bind(id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected()
            == 1;
        if revoked {
            self.query(
                "UPDATE mail_challenges SET revoked_at = ?
                 WHERE challenge_kind = 'INVITATION' AND reference_id = ?
                   AND consumed_at IS NULL AND revoked_at IS NULL AND created_at <= ?",
            )
            .bind(now)
            .bind(id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
            self.query(
                "UPDATE mail_outbox
                 SET status = 'FAILED', lease_expires_at = NULL,
                     last_error_code = 'REFERENCE_UNAVAILABLE', sent_at = NULL,
                     updated_at = ?
                 WHERE reference_id IN (
                     SELECT id FROM mail_challenges
                     WHERE challenge_kind = 'INVITATION' AND reference_id = ?
                 ) AND status IN ('PENDING', 'RETRY_WAIT', 'SENDING')",
            )
            .bind(now)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        }
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(revoked)
    }

    pub(crate) async fn get_mail_challenge(
        &self,
        id: &str,
    ) -> Result<Option<StoredMailChallenge>, StorageError> {
        self.query_as::<MailChallengeRecord>(
            "SELECT id, challenge_kind, recipient_email, reference_id, token_hash,
                    expires_at, consumed_at, revoked_at, created_at
             FROM mail_challenges WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .map(TryInto::try_into)
        .transpose()
    }

    pub(crate) async fn get_user_email_change_challenge_by_token_hash(
        &self,
        user_id: &str,
        token_hash: &[u8],
        now: i64,
    ) -> Result<Option<StoredMailChallenge>, StorageError> {
        if now < 0
            || user_id.trim() != user_id
            || !(1..=128).contains(&user_id.len())
            || user_id.chars().any(char::is_control)
            || token_hash.len() != 32
        {
            return Err(StorageError::Conflict(
                "invalid user email change challenge lookup".to_owned(),
            ));
        }
        self.query_as::<MailChallengeRecord>(
            "SELECT c.id, c.challenge_kind, c.recipient_email, c.reference_id,
                    c.token_hash, c.expires_at, c.consumed_at, c.revoked_at, c.created_at
             FROM mail_challenges c
             JOIN users u ON u.id = c.reference_id
             WHERE c.challenge_kind = 'EMAIL_VERIFICATION' AND c.reference_id = ?
               AND c.token_hash = ? AND c.expires_at > ? AND c.created_at <= ?
               AND c.consumed_at IS NULL AND c.revoked_at IS NULL
               AND u.is_disabled = 0 AND u.deleted_at IS NULL
             LIMIT 1",
        )
        .bind(user_id)
        .bind(token_hash)
        .bind(now)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .map(TryInto::try_into)
        .transpose()
    }

    pub(crate) async fn get_pending_user_email_change(
        &self,
        user_id: &str,
        now: i64,
    ) -> Result<Option<StoredMailChallenge>, StorageError> {
        if now < 0
            || user_id.trim() != user_id
            || !(1..=128).contains(&user_id.len())
            || user_id.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict(
                "invalid pending user email change lookup".to_owned(),
            ));
        }
        self.query_as::<MailChallengeRecord>(
            "SELECT c.id, c.challenge_kind, c.recipient_email, c.reference_id,
                    c.token_hash, c.expires_at, c.consumed_at, c.revoked_at, c.created_at
             FROM mail_challenges c
             JOIN users u ON u.id = c.reference_id
             WHERE c.challenge_kind = 'EMAIL_VERIFICATION' AND c.reference_id = ?
               AND c.expires_at > ? AND c.created_at <= ?
               AND c.consumed_at IS NULL AND c.revoked_at IS NULL
               AND u.is_disabled = 0 AND u.deleted_at IS NULL
             ORDER BY c.created_at DESC, c.id DESC LIMIT 1",
        )
        .bind(user_id)
        .bind(now)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .map(TryInto::try_into)
        .transpose()
    }

    #[cfg(test)]
    pub(crate) async fn consume_mail_challenge(
        &self,
        id: &str,
        token_hash: &[u8],
        now: i64,
    ) -> Result<bool, StorageError> {
        if now < 0 || token_hash.len() != 32 {
            return Err(StorageError::Conflict(
                "invalid mail challenge consumption".to_owned(),
            ));
        }
        let result = self
            .query(
                "UPDATE mail_challenges SET consumed_at = ?
                 WHERE id = ? AND token_hash = ? AND consumed_at IS NULL
                   AND revoked_at IS NULL AND expires_at > ? AND created_at <= ?",
            )
            .bind(now)
            .bind(id)
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }

    #[cfg(test)]
    pub(crate) async fn revoke_mail_challenges(
        &self,
        kind: MailChallengeKind,
        reference_id: &str,
        now: i64,
    ) -> Result<u64, StorageError> {
        if now < 0
            || reference_id.trim() != reference_id
            || !(1..=128).contains(&reference_id.len())
            || reference_id.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict(
                "invalid mail challenge revocation".to_owned(),
            ));
        }
        let result = self
            .query(
                "UPDATE mail_challenges SET revoked_at = ?
                 WHERE challenge_kind = ? AND reference_id = ?
                   AND consumed_at IS NULL AND revoked_at IS NULL AND created_at <= ?",
            )
            .bind(now)
            .bind(kind.as_str())
            .bind(reference_id)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected())
    }
}

fn user_registration_permission_snapshot(
    json: &str,
) -> Result<UserInvitationPermissionSnapshot, StorageError> {
    let snapshot: UserInvitationPermissionSnapshot = serde_json::from_str(json).map_err(|_| {
        StorageError::Conflict("invalid user registration permission snapshot".to_owned())
    })?;
    if json.len() > 32_768
        || !matches!(
            snapshot.permissions.library_access_mode.as_str(),
            "ALL" | "SELECTED" | "NONE"
        )
        || snapshot.permissions.max_simultaneous_playbacks < 0
        || snapshot.permissions.library_ids.iter().any(|id| {
            id.trim() != id || !(1..=128).contains(&id.len()) || id.chars().any(char::is_control)
        })
        || snapshot
            .permissions
            .library_ids
            .iter()
            .collect::<HashSet<_>>()
            .len()
            != snapshot.permissions.library_ids.len()
    {
        return Err(StorageError::Conflict(
            "invalid user registration permission snapshot".to_owned(),
        ));
    }
    Ok(snapshot)
}

impl Database {
    async fn cancel_user_registration_mail(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        request_id: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE mail_challenges SET revoked_at = ?
             WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
               AND consumed_at IS NULL AND revoked_at IS NULL AND created_at <= ?",
        )
        .bind(now)
        .bind(request_id)
        .bind(now)
        .execute(&mut **transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE mail_outbox
             SET status = 'FAILED', lease_expires_at = NULL,
                 last_error_code = 'REFERENCE_UNAVAILABLE', sent_at = NULL, updated_at = ?
             WHERE reference_id IN (
                 SELECT id FROM mail_challenges
                 WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
             ) AND status IN ('PENDING', 'RETRY_WAIT', 'SENDING')",
        )
        .bind(now)
        .bind(request_id)
        .execute(&mut **transaction)
        .await
        .map_err(self.sqlx_err())?;
        Ok(())
    }

    pub(crate) async fn create_user_registration_request_with_mail(
        &self,
        request: NewUserRegistrationRequest<'_>,
        challenge: NewMailChallenge<'_>,
        message: NewMailOutboxMessage<'_>,
        now: i64,
    ) -> Result<UserRegistrationCreateResult, StorageError> {
        let _snapshot = user_registration_permission_snapshot(request.permission_snapshot_json)?;
        if now < 0
            || request.id.trim() != request.id
            || !(1..=128).contains(&request.id.len())
            || request.id.chars().any(char::is_control)
            || request.recipient_email.trim() != request.recipient_email
            || request.recipient_email != request.recipient_email.to_lowercase()
            || !(3..=254).contains(&request.recipient_email.chars().count())
            || request.recipient_email.chars().any(char::is_whitespace)
            || request.username_normalized.trim() != request.username_normalized
            || request.username_normalized.is_empty()
            || request.username_normalized.chars().count() > 128
            || request.username_normalized != request.username_normalized.to_lowercase()
            || request.username_normalized.chars().any(char::is_control)
            || request.display_name.trim().is_empty()
            || request.display_name.chars().count() > 128
            || request.display_name.chars().any(char::is_control)
            || request.password_hash.len() > 2048
            || !request.password_hash.starts_with("$argon2id$")
            || request.expires_at <= now
            || challenge.id.trim() != challenge.id
            || !(1..=128).contains(&challenge.id.len())
            || challenge.id.chars().any(char::is_control)
            || challenge.kind != MailChallengeKind::EmailVerification
            || challenge.recipient_email != request.recipient_email
            || challenge.reference_id != request.id
            || challenge.token_hash.len() != 32
            || challenge.expires_at != request.expires_at
            || message.idempotency_key.trim() != message.idempotency_key
            || !(1..=256).contains(&message.idempotency_key.len())
            || message.idempotency_key.chars().any(char::is_control)
            || message.recipient_email != request.recipient_email
            || message.template_kind != MailOutboxTemplateKind::EmailVerification
            || message.reference_id != challenge.id
            || message.available_at != now
        {
            return Err(StorageError::Conflict(
                "invalid user registration and mail bundle".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let expired_ids: Vec<String> = self
            .query_scalar(
                "SELECT id FROM user_registration_requests
                 WHERE recipient_email = ?
                   AND status IN ('PENDING_EMAIL', 'PENDING_APPROVAL') AND expires_at <= ?",
            )
            .bind(request.recipient_email)
            .bind(now)
            .fetch_all(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        for expired_id in expired_ids {
            self.query(
                "UPDATE user_registration_requests
                 SET status = 'EXPIRED', password_hash = NULL, last_error_code = NULL, updated_at = ?
                 WHERE id = ? AND status IN ('PENDING_EMAIL', 'PENDING_APPROVAL') AND expires_at <= ?",
            )
            .bind(now)
            .bind(&expired_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
            self.cancel_user_registration_mail(&mut transaction, &expired_id, now)
                .await?;
        }

        let existing_user: Option<String> = self
            .query_scalar(
                "SELECT id FROM users WHERE email IS NOT NULL AND LOWER(email) = ? LIMIT 1",
            )
            .bind(request.recipient_email)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if existing_user.is_some() {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationCreateResult::AlreadyExists);
        }

        let recent_email_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM mail_outbox
                 WHERE recipient_email = ? AND template_kind = 'EMAIL_VERIFICATION'
                   AND created_at > ?",
            )
            .bind(request.recipient_email)
            .bind(now.saturating_sub(USER_REGISTRATION_MAIL_RATE_WINDOW_SECONDS))
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if recent_email_count >= USER_REGISTRATION_MAIL_RATE_LIMIT_PER_HOUR {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationCreateResult::RateLimited);
        }

        let inserted = self
            .query(
                "INSERT INTO user_registration_requests (
                     id, recipient_email, username_normalized, display_name, password_hash,
                     permission_snapshot_json, status, expires_at, created_at, updated_at
                 ) VALUES (?, ?, ?, ?, ?, ?, 'PENDING_EMAIL', ?, ?, ?)
                 ON CONFLICT DO NOTHING",
            )
            .bind(request.id)
            .bind(request.recipient_email)
            .bind(request.username_normalized)
            .bind(request.display_name)
            .bind(request.password_hash)
            .bind(request.permission_snapshot_json)
            .bind(request.expires_at)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if inserted.rows_affected() != 1 {
            let existing_request: Option<String> = self
                .query_scalar(
                    "SELECT id FROM user_registration_requests
                     WHERE recipient_email = ? AND status IN ('PENDING_EMAIL', 'PENDING_APPROVAL')
                       AND expires_at > ? LIMIT 1",
                )
                .bind(request.recipient_email)
                .bind(now)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            transaction.commit().await.map_err(self.sqlx_err())?;
            return if existing_request.is_some() {
                Ok(UserRegistrationCreateResult::AlreadyExists)
            } else {
                Err(StorageError::Conflict(
                    "user registration request identifier already exists".to_owned(),
                ))
            };
        }

        self.query(
            "INSERT INTO mail_challenges (
                 id, challenge_kind, recipient_email, reference_id, token_hash,
                 expires_at, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(challenge.id)
        .bind(challenge.kind.as_str())
        .bind(challenge.recipient_email)
        .bind(challenge.reference_id)
        .bind(challenge.token_hash)
        .bind(challenge.expires_at)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "INSERT INTO mail_outbox (
                 id, idempotency_key, recipient_email, template_kind, reference_id,
                 status, attempt_count, available_at, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 'PENDING', 0, ?, ?, ?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(message.idempotency_key)
        .bind(message.recipient_email)
        .bind(message.template_kind.as_str())
        .bind(message.reference_id)
        .bind(message.available_at)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserRegistrationCreateResult::Created)
    }
}

impl Database {
    pub(crate) async fn queue_user_registration_resend(
        &self,
        recipient_email: &str,
        idempotency_key: &str,
        now: i64,
    ) -> Result<UserRegistrationResendResult, StorageError> {
        if now < 0
            || recipient_email.trim() != recipient_email
            || recipient_email != recipient_email.to_lowercase()
            || !(3..=254).contains(&recipient_email.chars().count())
            || recipient_email
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || idempotency_key.trim() != idempotency_key
            || !(1..=256).contains(&idempotency_key.len())
            || idempotency_key.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict(
                "invalid user registration resend".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let request_statement = format!(
            "SELECT id, expires_at FROM user_registration_requests
             WHERE recipient_email = ? AND status = 'PENDING_EMAIL'
               AND email_verified_at IS NULL AND password_hash IS NOT NULL
               AND expires_at > ? AND created_at <= ?
             ORDER BY created_at DESC, id DESC LIMIT 1{lock_clause}"
        );
        let request: Option<(String, i64)> = self
            .query_as::<(String, i64)>(sqlx::AssertSqlSafe(request_statement))
            .bind(recipient_email)
            .bind(now)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some((request_id, request_expires_at)) = request else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationResendResult::NotResendable);
        };

        let challenge_statement = format!(
            "SELECT id FROM mail_challenges
             WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
               AND recipient_email = ? AND expires_at = ? AND expires_at > ?
               AND created_at <= ? AND consumed_at IS NULL AND revoked_at IS NULL
             ORDER BY created_at DESC, id DESC LIMIT 1{lock_clause}"
        );
        let challenge_id: Option<String> = self
            .query_scalar(sqlx::AssertSqlSafe(challenge_statement))
            .bind(&request_id)
            .bind(recipient_email)
            .bind(request_expires_at)
            .bind(now)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some(challenge_id) = challenge_id else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationResendResult::NotResendable);
        };

        let existing_message: Option<(String, String, String)> = self
            .query_as(
                "SELECT recipient_email, template_kind, reference_id FROM mail_outbox
                 WHERE idempotency_key = ?",
            )
            .bind(idempotency_key)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if let Some((existing_email, existing_template, existing_reference)) = existing_message {
            if existing_email != recipient_email
                || existing_template != MailOutboxTemplateKind::EmailVerification.as_str()
                || existing_reference != challenge_id
            {
                return Err(StorageError::Conflict(
                    "mail outbox idempotency key already used".to_owned(),
                ));
            }
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationResendResult::Queued);
        }

        let recent_email_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM mail_outbox
                 WHERE recipient_email = ? AND template_kind = 'EMAIL_VERIFICATION'
                   AND created_at > ?",
            )
            .bind(recipient_email)
            .bind(now.saturating_sub(USER_REGISTRATION_MAIL_RATE_WINDOW_SECONDS))
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if recent_email_count >= USER_REGISTRATION_MAIL_RATE_LIMIT_PER_HOUR {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationResendResult::RateLimited);
        }

        self.query(
            "INSERT INTO mail_outbox (
                 id, idempotency_key, recipient_email, template_kind, reference_id,
                 status, attempt_count, available_at, created_at, updated_at
             ) VALUES (?, ?, ?, 'EMAIL_VERIFICATION', ?, 'PENDING', 0, ?, ?, ?)
             ON CONFLICT (idempotency_key) DO NOTHING",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(idempotency_key)
        .bind(recipient_email)
        .bind(&challenge_id)
        .bind(now)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;

        let stored_message: (String, String, String) = self
            .query_as(
                "SELECT recipient_email, template_kind, reference_id FROM mail_outbox
                 WHERE idempotency_key = ?",
            )
            .bind(idempotency_key)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if stored_message.0 != recipient_email
            || stored_message.1 != MailOutboxTemplateKind::EmailVerification.as_str()
            || stored_message.2 != challenge_id
        {
            return Err(StorageError::Conflict(
                "mail outbox idempotency key already used".to_owned(),
            ));
        }

        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserRegistrationResendResult::Queued)
    }

    pub(crate) async fn get_user_registration_challenge_by_token_hash(
        &self,
        token_hash: &[u8],
        now: i64,
    ) -> Result<Option<StoredUserRegistrationChallenge>, StorageError> {
        if now < 0 || token_hash.len() != 32 {
            return Err(StorageError::Conflict(
                "invalid user registration challenge lookup".to_owned(),
            ));
        }
        let record: Option<(String, i64)> = self
            .query_as(
                "SELECT c.id, c.expires_at
                 FROM mail_challenges c
                 JOIN user_registration_requests r ON r.id = c.reference_id
                 WHERE c.challenge_kind = 'EMAIL_VERIFICATION' AND c.token_hash = ?
                   AND c.consumed_at IS NULL AND c.revoked_at IS NULL
                   AND c.expires_at > ? AND c.created_at <= ?
                   AND r.status = 'PENDING_EMAIL' AND r.expires_at > ?
                   AND r.recipient_email = c.recipient_email AND r.expires_at = c.expires_at
                 ORDER BY c.created_at DESC, c.id DESC LIMIT 1",
            )
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .bind(now)
            .fetch_optional(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(record.map(|(id, expires_at)| StoredUserRegistrationChallenge { id, expires_at }))
    }

    pub(crate) async fn verify_user_registration_email(
        &self,
        challenge_id: &str,
        token_hash: &[u8],
        now: i64,
    ) -> Result<UserRegistrationVerifyResult, StorageError> {
        if now < 0
            || challenge_id.trim() != challenge_id
            || !(1..=128).contains(&challenge_id.len())
            || challenge_id.chars().any(char::is_control)
            || token_hash.len() != 32
        {
            return Err(StorageError::Conflict(
                "invalid user registration verification".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE OF r, c"
        } else {
            ""
        };
        let statement = format!(
            "SELECT r.id FROM user_registration_requests r
             JOIN mail_challenges c ON c.reference_id = r.id
             WHERE c.id = ? AND c.challenge_kind = 'EMAIL_VERIFICATION'
               AND c.token_hash = ? AND c.expires_at > ? AND c.created_at <= ?
               AND c.consumed_at IS NULL AND c.revoked_at IS NULL
               AND r.status = 'PENDING_EMAIL' AND r.expires_at > ?
               AND r.recipient_email = c.recipient_email AND r.expires_at = c.expires_at{lock_clause}"
        );
        let request_id: Option<String> = self
            .query_scalar(sqlx::AssertSqlSafe(statement))
            .bind(challenge_id)
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some(request_id) = request_id else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationVerifyResult::NotAcceptable);
        };

        let consumed = self
            .query(
                "UPDATE mail_challenges SET consumed_at = ?
                 WHERE id = ? AND challenge_kind = 'EMAIL_VERIFICATION' AND token_hash = ?
                   AND consumed_at IS NULL AND revoked_at IS NULL
                   AND expires_at > ? AND created_at <= ?",
            )
            .bind(now)
            .bind(challenge_id)
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        let transitioned = self
            .query(
                "UPDATE user_registration_requests
                 SET status = 'PENDING_APPROVAL', email_verified_at = ?, updated_at = ?,
                     last_error_code = NULL
                 WHERE id = ? AND status = 'PENDING_EMAIL' AND expires_at > ?
                   AND email_verified_at IS NULL",
            )
            .bind(now)
            .bind(now)
            .bind(&request_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        if consumed != 1 || transitioned != 1 {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationVerifyResult::NotAcceptable);
        }
        self.cancel_user_registration_mail(&mut transaction, &request_id, now)
            .await?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserRegistrationVerifyResult::Verified)
    }

    pub(crate) async fn verify_user_email_change(
        &self,
        user_id: &str,
        challenge_id: &str,
        token_hash: &[u8],
        now: i64,
    ) -> Result<crate::storage::UserEmailChangeVerifyResult, StorageError> {
        if now < 0
            || user_id.trim() != user_id
            || !(1..=128).contains(&user_id.len())
            || user_id.chars().any(char::is_control)
            || challenge_id.trim() != challenge_id
            || !(1..=128).contains(&challenge_id.len())
            || challenge_id.chars().any(char::is_control)
            || token_hash.len() != 32
        {
            return Err(StorageError::Conflict(
                "invalid user email change verification".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let user_statement = format!(
            "SELECT id FROM users WHERE id = ? AND is_disabled = 0
             AND deleted_at IS NULL{lock_clause}"
        );
        let active_user_id: Option<String> = self
            .query_scalar(sqlx::AssertSqlSafe(user_statement))
            .bind(user_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if active_user_id.is_none() {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(crate::storage::UserEmailChangeVerifyResult::NotAcceptable);
        }

        let challenge_statement = format!(
            "SELECT recipient_email FROM mail_challenges
             WHERE id = ? AND challenge_kind = 'EMAIL_VERIFICATION'
               AND reference_id = ? AND token_hash = ?
               AND expires_at > ? AND created_at <= ?
               AND consumed_at IS NULL AND revoked_at IS NULL{lock_clause}"
        );
        let recipient_email: Option<String> = self
            .query_scalar(sqlx::AssertSqlSafe(challenge_statement))
            .bind(challenge_id)
            .bind(user_id)
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some(recipient_email) = recipient_email else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(crate::storage::UserEmailChangeVerifyResult::NotAcceptable);
        };

        let email_in_use: i64 = self
            .query_scalar(
                "SELECT CASE WHEN EXISTS (
                     SELECT 1 FROM users WHERE id <> ? AND LOWER(COALESCE(email, '')) = LOWER(?)
                 ) THEN 1 ELSE 0 END",
            )
            .bind(user_id)
            .bind(&recipient_email)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if email_in_use != 0 {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(crate::storage::UserEmailChangeVerifyResult::EmailConflict);
        }

        let updated_user = self
            .query(
                "UPDATE users SET email = ?, email_verified_at = ?, updated_at = ?
                 WHERE id = ? AND is_disabled = 0 AND deleted_at IS NULL",
            )
            .bind(&recipient_email)
            .bind(now)
            .bind(now)
            .bind(user_id)
            .execute(&mut *transaction)
            .await;
        let updated_user = match updated_user {
            Ok(result) => result.rows_affected(),
            Err(source)
                if source
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation()) =>
            {
                transaction.rollback().await.map_err(self.sqlx_err())?;
                return Ok(crate::storage::UserEmailChangeVerifyResult::EmailConflict);
            }
            Err(source) => {
                return Err(StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        let consumed_challenge = self
            .query(
                "UPDATE mail_challenges SET consumed_at = ?
                 WHERE id = ? AND challenge_kind = 'EMAIL_VERIFICATION'
                   AND reference_id = ? AND token_hash = ?
                   AND consumed_at IS NULL AND revoked_at IS NULL
                   AND expires_at > ? AND created_at <= ?",
            )
            .bind(now)
            .bind(challenge_id)
            .bind(user_id)
            .bind(token_hash)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        if updated_user != 1 || consumed_challenge != 1 {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(crate::storage::UserEmailChangeVerifyResult::NotAcceptable);
        }

        self.query(
            "UPDATE mail_challenges SET revoked_at = ?
             WHERE challenge_kind = 'EMAIL_VERIFICATION' AND reference_id = ?
               AND id <> ? AND consumed_at IS NULL AND revoked_at IS NULL
               AND created_at <= ?",
        )
        .bind(now)
        .bind(user_id)
        .bind(challenge_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(crate::storage::UserEmailChangeVerifyResult::Verified)
    }

    pub(crate) async fn list_user_registration_requests_page(
        &self,
        status: Option<UserRegistrationStatus>,
        page_size: i64,
        offset: i64,
        now: i64,
    ) -> Result<UserRegistrationRequestPage, StorageError> {
        if now < 0 {
            return Err(StorageError::Conflict(
                "invalid user registration page time".to_owned(),
            ));
        }
        let page_size = page_size.clamp(1, 100);
        let offset = offset.max(0);
        let status_filter = if status.is_some() {
            " WHERE status = ?"
        } else {
            ""
        };
        let count_statement = format!(
            "SELECT COUNT(*) FROM (
                 SELECT CASE
                     WHEN status IN ('PENDING_EMAIL', 'PENDING_APPROVAL') AND expires_at <= ?
                         THEN 'EXPIRED' ELSE status END AS status
                 FROM user_registration_requests
             ) AS requests{status_filter}"
        );
        let mut count_query = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_statement));
        count_query = count_query.bind(now);
        if let Some(status) = status {
            count_query = count_query.bind(status.as_str());
        }
        let total_count = count_query
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;

        let page_statement = format!(
            "SELECT id, recipient_email, username_normalized, display_name,
                    permission_snapshot_json, status, expires_at, email_verified_at,
                    reviewed_at, reviewed_by_user_id, approved_user_id, last_error_code,
                    created_at, updated_at
             FROM (
                 SELECT id, recipient_email, username_normalized, display_name,
                        permission_snapshot_json,
                        CASE WHEN status IN ('PENDING_EMAIL', 'PENDING_APPROVAL') AND expires_at <= ?
                             THEN 'EXPIRED' ELSE status END AS status,
                        expires_at, email_verified_at, reviewed_at, reviewed_by_user_id,
                        approved_user_id, last_error_code, created_at, updated_at
                 FROM user_registration_requests
             ) AS requests{status_filter}
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        );
        let mut page_query = self
            .query_as::<UserRegistrationRequestSummaryRecord>(sqlx::AssertSqlSafe(page_statement))
            .bind(now);
        if let Some(status) = status {
            page_query = page_query.bind(status.as_str());
        }
        let records = page_query
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let requests = records
            .into_iter()
            .map(|record| {
                Ok(StoredUserRegistrationRequest {
                    id: record.id,
                    recipient_email: record.recipient_email,
                    username_normalized: record.username_normalized,
                    display_name: record.display_name,
                    status: UserRegistrationStatus::parse(&record.status)?,
                    permission_snapshot_json: record.permission_snapshot_json,
                    expires_at: record.expires_at,
                    email_verified_at: record.email_verified_at,
                    reviewed_at: record.reviewed_at,
                    reviewed_by_user_id: record.reviewed_by_user_id,
                    approved_user_id: record.approved_user_id,
                    last_error_code: record.last_error_code,
                    created_at: record.created_at,
                    updated_at: record.updated_at,
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        Ok(UserRegistrationRequestPage {
            requests,
            page_size,
            total_count,
        })
    }

    async fn mark_user_registration_error(
        &self,
        request_id: &str,
        error_code: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE user_registration_requests
             SET last_error_code = ?, updated_at = ?
             WHERE id = ? AND status = 'PENDING_APPROVAL' AND expires_at > ?
               AND email_verified_at IS NOT NULL",
        )
        .bind(error_code)
        .bind(now)
        .bind(request_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(self.sqlx_err())?;
        Ok(())
    }

    pub(crate) async fn approve_user_registration_request(
        &self,
        request_id: &str,
        reviewer_user_id: &str,
        permission_snapshot_json: &str,
        now: i64,
    ) -> Result<UserRegistrationApproveResult, StorageError> {
        let permission_snapshot = user_registration_permission_snapshot(permission_snapshot_json)?;
        if now < 0
            || request_id.trim() != request_id
            || !(1..=128).contains(&request_id.len())
            || request_id.chars().any(char::is_control)
            || reviewer_user_id.trim() != reviewer_user_id
            || !(1..=128).contains(&reviewer_user_id.len())
            || reviewer_user_id.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict(
                "invalid user registration approval".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let request_statement = format!(
            "SELECT recipient_email, username_normalized, display_name, password_hash,
                    email_verified_at
             FROM user_registration_requests
             WHERE id = ? AND status = 'PENDING_APPROVAL' AND email_verified_at IS NOT NULL
               AND expires_at > ?{lock_clause}"
        );
        let request = self
            .query_as::<UserRegistrationRequestRecord>(sqlx::AssertSqlSafe(request_statement))
            .bind(request_id)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some(request) = request else {
            let expired = self
                .query(
                    "UPDATE user_registration_requests
                     SET status = 'EXPIRED', password_hash = NULL,
                         last_error_code = NULL, updated_at = ?
                     WHERE id = ? AND status = 'PENDING_APPROVAL' AND expires_at <= ?",
                )
                .bind(now)
                .bind(request_id)
                .bind(now)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?
                .rows_affected();
            if expired == 1 {
                self.cancel_user_registration_mail(&mut transaction, request_id, now)
                    .await?;
            }
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationApproveResult::NotReviewable);
        };
        let Some(email_verified_at) = request.email_verified_at else {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationApproveResult::NotReviewable);
        };
        if now < email_verified_at {
            return Err(StorageError::Conflict(
                "registration approval time precedes email verification".to_owned(),
            ));
        }
        let Some(password_hash) = request.password_hash.as_deref() else {
            return Err(StorageError::Serialization(
                "pending user registration has no password hash".to_owned(),
            ));
        };

        let username_taken = self
            .query_scalar::<String>("SELECT id FROM users WHERE username_normalized = ? LIMIT 1")
            .bind(&request.username_normalized)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .is_some();
        if username_taken {
            self.query(
                "UPDATE user_registration_requests
                 SET last_error_code = 'USERNAME_CONFLICT', updated_at = ? WHERE id = ?",
            )
            .bind(now)
            .bind(request_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationApproveResult::UsernameConflict);
        }
        let email_taken = self
            .query_scalar::<String>(
                "SELECT id FROM users WHERE email IS NOT NULL AND LOWER(email) = ? LIMIT 1",
            )
            .bind(&request.recipient_email)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .is_some();
        if email_taken {
            self.query(
                "UPDATE user_registration_requests
                 SET last_error_code = 'EMAIL_CONFLICT', updated_at = ? WHERE id = ?",
            )
            .bind(now)
            .bind(request_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationApproveResult::EmailConflict);
        }

        let user_id = Uuid::now_v7().to_string();
        let permissions = permission_snapshot.permissions;
        let inserted_user = self
            .query(
                "INSERT INTO users (
                    id, username_normalized, display_name, password_hash,
                    is_admin, can_manage_server, has_password, email, email_verified_at,
                    can_download, can_play, max_simultaneous_playbacks,
                    allow_remux, allow_audio_transcode, allow_video_transcode,
                    library_access_mode
                 ) VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&user_id)
            .bind(&request.username_normalized)
            .bind(&request.display_name)
            .bind(password_hash)
            .bind(database_flag(permission_snapshot.is_admin))
            .bind(database_flag(permission_snapshot.can_manage_server))
            .bind(&request.recipient_email)
            .bind(email_verified_at)
            .bind(database_flag(permissions.can_download))
            .bind(database_flag(permissions.can_play))
            .bind(permissions.max_simultaneous_playbacks)
            .bind(database_flag(permissions.allow_remux))
            .bind(database_flag(permissions.allow_audio_transcode))
            .bind(database_flag(permissions.allow_video_transcode))
            .bind(&permissions.library_access_mode)
            .execute(&mut *transaction)
            .await;
        if let Err(source) = inserted_user {
            let unique_violation = source
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation());
            transaction.rollback().await.map_err(self.sqlx_err())?;
            if unique_violation {
                let username_taken = self
                    .query_scalar::<String>(
                        "SELECT id FROM users WHERE username_normalized = ? LIMIT 1",
                    )
                    .bind(&request.username_normalized)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(self.sqlx_err())?
                    .is_some();
                if username_taken {
                    self.mark_user_registration_error(request_id, "USERNAME_CONFLICT", now)
                        .await?;
                    return Ok(UserRegistrationApproveResult::UsernameConflict);
                }
                let email_taken = self
                    .query_scalar::<String>(
                        "SELECT id FROM users WHERE email IS NOT NULL AND LOWER(email) = ? LIMIT 1",
                    )
                    .bind(&request.recipient_email)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(self.sqlx_err())?
                    .is_some();
                if email_taken {
                    self.mark_user_registration_error(request_id, "EMAIL_CONFLICT", now)
                        .await?;
                    return Ok(UserRegistrationApproveResult::EmailConflict);
                }
            }
            return Err(StorageError::Sqlx {
                path: self.path.clone(),
                source,
            });
        }

        if permissions.library_access_mode == "SELECTED" {
            for library_id in &permissions.library_ids {
                if let Err(source) = self
                    .query(
                        "INSERT INTO user_library_access (user_id, library_id, can_view)
                         VALUES (?, ?, 1)",
                    )
                    .bind(&user_id)
                    .bind(library_id)
                    .execute(&mut *transaction)
                    .await
                {
                    transaction.rollback().await.map_err(self.sqlx_err())?;
                    return Err(StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    });
                }
            }
        }
        let updated = self
            .query(
                "UPDATE user_registration_requests
                 SET status = 'APPROVED', permission_snapshot_json = ?,
                     reviewed_at = ?, reviewed_by_user_id = ?, approved_user_id = ?,
                     password_hash = NULL, last_error_code = NULL, updated_at = ?
                 WHERE id = ? AND status = 'PENDING_APPROVAL' AND expires_at > ?",
            )
            .bind(permission_snapshot_json)
            .bind(now)
            .bind(reviewer_user_id)
            .bind(&user_id)
            .bind(now)
            .bind(request_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        if updated != 1 {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationApproveResult::NotReviewable);
        }
        self.cancel_user_registration_mail(&mut transaction, request_id, now)
            .await?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserRegistrationApproveResult::Approved(user_id))
    }
}

impl Database {
    pub(crate) async fn reject_user_registration_request(
        &self,
        request_id: &str,
        reviewer_user_id: &str,
        now: i64,
    ) -> Result<UserRegistrationRejectResult, StorageError> {
        if now < 0
            || request_id.trim() != request_id
            || !(1..=128).contains(&request_id.len())
            || request_id.chars().any(char::is_control)
            || reviewer_user_id.trim() != reviewer_user_id
            || !(1..=128).contains(&reviewer_user_id.len())
            || reviewer_user_id.chars().any(char::is_control)
        {
            return Err(StorageError::Conflict(
                "invalid user registration rejection".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let statement = format!(
            "SELECT id FROM user_registration_requests
             WHERE id = ? AND status = 'PENDING_APPROVAL' AND email_verified_at IS NOT NULL
               AND expires_at > ?{lock_clause}"
        );
        let reviewable: Option<String> = self
            .query_scalar(sqlx::AssertSqlSafe(statement))
            .bind(request_id)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if reviewable.is_none() {
            let expired = self
                .query(
                    "UPDATE user_registration_requests
                     SET status = 'EXPIRED', password_hash = NULL,
                         last_error_code = NULL, updated_at = ?
                     WHERE id = ? AND status = 'PENDING_APPROVAL' AND expires_at <= ?",
                )
                .bind(now)
                .bind(request_id)
                .bind(now)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?
                .rows_affected();
            if expired == 1 {
                self.cancel_user_registration_mail(&mut transaction, request_id, now)
                    .await?;
            }
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationRejectResult::NotReviewable);
        }

        let updated = self
            .query(
                "UPDATE user_registration_requests
                 SET status = 'REJECTED', reviewed_at = ?, reviewed_by_user_id = ?,
                     password_hash = NULL, last_error_code = NULL, updated_at = ?
                 WHERE id = ? AND status = 'PENDING_APPROVAL' AND expires_at > ?",
            )
            .bind(now)
            .bind(reviewer_user_id)
            .bind(now)
            .bind(request_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        if updated != 1 {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(UserRegistrationRejectResult::NotReviewable);
        }
        self.cancel_user_registration_mail(&mut transaction, request_id, now)
            .await?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(UserRegistrationRejectResult::Rejected)
    }

    #[cfg(test)]
    pub(crate) async fn expire_user_registration_requests(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<u64, StorageError> {
        if now < 0 {
            return Err(StorageError::Conflict(
                "invalid user registration expiry time".to_owned(),
            ));
        }
        let limit = limit.clamp(1, 100);
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE SKIP LOCKED"
        } else {
            ""
        };
        let statement = format!(
            "SELECT id FROM user_registration_requests
             WHERE status IN ('PENDING_EMAIL', 'PENDING_APPROVAL') AND expires_at <= ?
             ORDER BY expires_at, id LIMIT ?{lock_clause}"
        );
        let expired_ids: Vec<String> = self
            .query_scalar(sqlx::AssertSqlSafe(statement))
            .bind(now)
            .bind(limit)
            .fetch_all(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let mut expired_count = 0_u64;
        for request_id in expired_ids {
            let updated = self
                .query(
                    "UPDATE user_registration_requests
                     SET status = 'EXPIRED', password_hash = NULL,
                         last_error_code = NULL, updated_at = ?
                     WHERE id = ? AND status IN ('PENDING_EMAIL', 'PENDING_APPROVAL')
                       AND expires_at <= ?",
                )
                .bind(now)
                .bind(&request_id)
                .bind(now)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?
                .rows_affected();
            if updated == 1 {
                self.cancel_user_registration_mail(&mut transaction, &request_id, now)
                    .await?;
                expired_count += 1;
            }
        }
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(expired_count)
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RegistrationModeRecord {
    mode: crate::storage::RegistrationMode,
    version: i64,
}

impl Database {
    pub(crate) async fn registration_mode(
        &self,
    ) -> Result<crate::storage::StoredRegistrationMode, StorageError> {
        let raw_value: Option<String> = self
            .query_scalar("SELECT value FROM server_settings WHERE key = 'registration_mode'")
            .fetch_optional(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let parsed = raw_value
            .as_deref()
            .and_then(|value| serde_json::from_str::<RegistrationModeRecord>(value).ok())
            .filter(|record| record.version > 0);
        let (mode, version) = parsed.map_or(
            (crate::storage::RegistrationMode::InvitationOnly, 0),
            |record| (record.mode, record.version),
        );
        Ok(crate::storage::StoredRegistrationMode {
            mode,
            version,
            raw_value,
        })
    }

    pub(crate) async fn compare_and_set_registration_mode(
        &self,
        expected: &crate::storage::StoredRegistrationMode,
        mode: crate::storage::RegistrationMode,
    ) -> Result<bool, StorageError> {
        let version = expected.version.checked_add(1).ok_or_else(|| {
            StorageError::Conflict("registration setting version exhausted".to_owned())
        })?;
        let value =
            serde_json::to_string(&RegistrationModeRecord { mode, version }).map_err(|_| {
                StorageError::Serialization("registration setting serialization failed".to_owned())
            })?;
        let result = if let Some(raw_value) = expected.raw_value.as_deref() {
            self.query(
                "UPDATE server_settings SET value = ?, updated_at = unixepoch()
                        WHERE key = 'registration_mode' AND value = ?",
            )
            .bind(&value)
            .bind(raw_value)
            .execute(&self.pool)
            .await
        } else {
            self.query(
                "INSERT INTO server_settings (key, value) VALUES ('registration_mode', ?)
                        ON CONFLICT(key) DO NOTHING",
            )
            .bind(&value)
            .execute(&self.pool)
            .await
        }
        .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }
}
