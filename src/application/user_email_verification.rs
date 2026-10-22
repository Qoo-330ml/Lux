use std::{error::Error, fmt, path::Path, time::Duration};

use lettre::Address;
use uuid::Uuid;

use crate::{
    application::mail::{MailChallengeSigner, hash_mail_challenge_token},
    security::DevicePairingRateLimiter,
    storage::{
        Database, MailChallengeKind, MailOutboxTemplateKind, NewMailChallenge,
        NewMailOutboxMessage, UserEmailChangeCreateResult, UserEmailChangeVerifyResult,
    },
};

const USER_EMAIL_CHANGE_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const USER_EMAIL_CHANGE_RATE_LIMIT: u32 = 5;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum UserEmailVerificationError {
    InvalidEmail,
    InvalidInput,
    NotAcceptable,
    RateLimited,
    SigningUnavailable,
    StorageUnavailable,
}

impl fmt::Display for UserEmailVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEmail => formatter.write_str("email address is invalid"),
            Self::InvalidInput => formatter.write_str("email verification input is invalid"),
            Self::NotAcceptable => formatter.write_str("email verification is not acceptable"),
            Self::RateLimited => formatter.write_str("email verification rate limit exceeded"),
            Self::SigningUnavailable => {
                formatter.write_str("email verification signing unavailable")
            }
            Self::StorageUnavailable => {
                formatter.write_str("email verification storage unavailable")
            }
        }
    }
}

impl Error for UserEmailVerificationError {}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PendingUserEmailChange {
    pub(crate) challenge_id: String,
    pub(crate) pending_email: String,
    pub(crate) expires_at: i64,
}

pub(crate) async fn clear_user_email_change(
    database: &Database,
    user_id: &str,
    now: i64,
) -> Result<(), UserEmailVerificationError> {
    if now < 0
        || user_id.trim() != user_id
        || !(1..=128).contains(&user_id.len())
        || user_id.chars().any(char::is_control)
    {
        return Err(UserEmailVerificationError::InvalidInput);
    }
    database
        .clear_user_email_change(user_id, now)
        .await
        .map_err(|_| UserEmailVerificationError::StorageUnavailable)?
        .then_some(())
        .ok_or(UserEmailVerificationError::NotAcceptable)
}

pub(crate) async fn request_user_email_change(
    database: &Database,
    config_dir: &Path,
    rate_limiter: &DevicePairingRateLimiter,
    user_id: &str,
    email: &str,
    now: i64,
) -> Result<PendingUserEmailChange, UserEmailVerificationError> {
    request_user_email_change_with_expected_pending(
        database,
        config_dir,
        rate_limiter,
        user_id,
        email,
        None,
        now,
    )
    .await?
    .ok_or(UserEmailVerificationError::NotAcceptable)
}

pub(crate) async fn resend_user_email_change(
    database: &Database,
    config_dir: &Path,
    rate_limiter: &DevicePairingRateLimiter,
    user_id: &str,
    pending: &PendingUserEmailChange,
    now: i64,
) -> Result<(), UserEmailVerificationError> {
    request_user_email_change_with_expected_pending(
        database,
        config_dir,
        rate_limiter,
        user_id,
        &pending.pending_email,
        Some(&pending.challenge_id),
        now,
    )
    .await
    .map(|_| ())
}

async fn request_user_email_change_with_expected_pending(
    database: &Database,
    config_dir: &Path,
    rate_limiter: &DevicePairingRateLimiter,
    user_id: &str,
    email: &str,
    expected_pending_challenge_id: Option<&str>,
    now: i64,
) -> Result<Option<PendingUserEmailChange>, UserEmailVerificationError> {
    if now < 0
        || user_id.trim() != user_id
        || !(1..=128).contains(&user_id.len())
        || user_id.chars().any(char::is_control)
    {
        return Err(UserEmailVerificationError::InvalidInput);
    }
    let pending_email = normalize_email(email)?;
    let user = database
        .find_user_by_id(user_id)
        .await
        .map_err(|_| UserEmailVerificationError::StorageUnavailable)?
        .filter(|user| !user.is_disabled && user.deleted_at.is_none())
        .ok_or(UserEmailVerificationError::NotAcceptable)?;
    if expected_pending_challenge_id.is_none()
        && user.email.as_deref().is_some_and(|current| {
            user.email_verified_at.is_some() && current.eq_ignore_ascii_case(&pending_email)
        })
    {
        return Err(UserEmailVerificationError::NotAcceptable);
    }

    if !rate_limiter
        .try_acquire(
            &format!("user-email-change:account:{user_id}"),
            USER_EMAIL_CHANGE_RATE_LIMIT,
        )
        .await
        || !rate_limiter
            .try_acquire(
                &format!("user-email-change:recipient:{pending_email}"),
                USER_EMAIL_CHANGE_RATE_LIMIT,
            )
            .await
    {
        return Err(UserEmailVerificationError::RateLimited);
    }

    let expires_at = now
        .checked_add(USER_EMAIL_CHANGE_LIFETIME.as_secs() as i64)
        .ok_or(UserEmailVerificationError::InvalidInput)?;
    let challenge_id = Uuid::now_v7().to_string();
    let signer = MailChallengeSigner::load_or_create(config_dir)
        .await
        .map_err(|_| UserEmailVerificationError::SigningUnavailable)?;
    let token = signer
        .token_for(
            &challenge_id,
            MailChallengeKind::EmailVerification,
            expires_at,
        )
        .ok_or(UserEmailVerificationError::SigningUnavailable)?;
    let token_hash = hash_mail_challenge_token(&token);
    let idempotency_key = format!("user-email-change:{challenge_id}");
    match database
        .create_user_email_change_with_mail(
            user_id,
            NewMailChallenge {
                id: &challenge_id,
                kind: MailChallengeKind::EmailVerification,
                recipient_email: &pending_email,
                reference_id: user_id,
                token_hash: &token_hash,
                expires_at,
            },
            NewMailOutboxMessage {
                idempotency_key: &idempotency_key,
                recipient_email: &pending_email,
                template_kind: MailOutboxTemplateKind::EmailVerification,
                reference_id: &challenge_id,
                available_at: now,
            },
            expected_pending_challenge_id,
            now,
        )
        .await
        .map_err(|_| UserEmailVerificationError::StorageUnavailable)?
    {
        UserEmailChangeCreateResult::Created => Ok(Some(PendingUserEmailChange {
            challenge_id,
            pending_email,
            expires_at,
        })),
        UserEmailChangeCreateResult::NotAcceptable if expected_pending_challenge_id.is_some() => {
            Ok(None)
        }
        UserEmailChangeCreateResult::NotAcceptable => {
            Err(UserEmailVerificationError::NotAcceptable)
        }
    }
}

pub(crate) async fn pending_user_email_change(
    database: &Database,
    user_id: &str,
    now: i64,
) -> Result<Option<PendingUserEmailChange>, UserEmailVerificationError> {
    if now < 0
        || user_id.trim() != user_id
        || !(1..=128).contains(&user_id.len())
        || user_id.chars().any(char::is_control)
    {
        return Err(UserEmailVerificationError::InvalidInput);
    }
    database
        .get_pending_user_email_change(user_id, now)
        .await
        .map(|challenge| {
            challenge.map(|challenge| PendingUserEmailChange {
                challenge_id: challenge.id,
                pending_email: challenge.recipient_email,
                expires_at: challenge.expires_at,
            })
        })
        .map_err(|_| UserEmailVerificationError::StorageUnavailable)
}

pub(crate) async fn verify_user_email_change(
    database: &Database,
    config_dir: &Path,
    user_id: &str,
    token: &str,
    now: i64,
) -> Result<(), UserEmailVerificationError> {
    if now < 0
        || user_id.trim() != user_id
        || !(1..=128).contains(&user_id.len())
        || user_id.chars().any(char::is_control)
        || token.len() != 43
    {
        return Err(UserEmailVerificationError::NotAcceptable);
    }
    let token_hash = hash_mail_challenge_token(token);
    let Some(challenge) = database
        .get_user_email_change_challenge_by_token_hash(user_id, &token_hash, now)
        .await
        .map_err(|_| UserEmailVerificationError::StorageUnavailable)?
    else {
        return Err(UserEmailVerificationError::NotAcceptable);
    };
    let signer = MailChallengeSigner::load_or_create(config_dir)
        .await
        .map_err(|_| UserEmailVerificationError::SigningUnavailable)?;
    if !signer.verify(
        &challenge.id,
        MailChallengeKind::EmailVerification,
        challenge.expires_at,
        token,
    ) {
        return Err(UserEmailVerificationError::NotAcceptable);
    }
    match database
        .verify_user_email_change(user_id, &challenge.id, &token_hash, now)
        .await
        .map_err(|_| UserEmailVerificationError::StorageUnavailable)?
    {
        UserEmailChangeVerifyResult::Verified => Ok(()),
        UserEmailChangeVerifyResult::NotAcceptable | UserEmailChangeVerifyResult::EmailConflict => {
            Err(UserEmailVerificationError::NotAcceptable)
        }
    }
}

fn normalize_email(value: &str) -> Result<String, UserEmailVerificationError> {
    if value.chars().any(char::is_control) {
        return Err(UserEmailVerificationError::InvalidEmail);
    }
    let normalized = value.trim().to_lowercase();
    if !(3..=254).contains(&normalized.len()) || normalized.parse::<Address>().is_err() {
        return Err(UserEmailVerificationError::InvalidEmail);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use crate::{
        application::{
            mail::MailChallengeSigner,
            user_email_verification::{
                UserEmailVerificationError, pending_user_email_change, request_user_email_change,
                verify_user_email_change,
            },
        },
        config::Config,
        security::DevicePairingRateLimiter,
        storage::{Database, MailChallengeKind, NewStoredUser},
    };

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        Ok((directory, database))
    }

    async fn add_user(database: &Database, id: &str, email: &str) -> Result<(), Box<dyn Error>> {
        database
            .insert_user_with_email(NewStoredUser {
                id,
                username_normalized: id,
                display_name: id,
                password_hash: "test-hash",
                is_admin: false,
                has_password: true,
                email: Some(email),
            })
            .await?;
        sqlx::query("UPDATE users SET email_verified_at = 999 WHERE id = ?")
            .bind(id)
            .execute(database.pool())
            .await?;
        Ok(())
    }

    async fn challenge_token(
        database: &Database,
        config_dir: &std::path::Path,
        user_id: &str,
        recipient_email: &str,
    ) -> Result<(String, i64), Box<dyn Error>> {
        let (challenge_id, expires_at): (String, i64) = sqlx::query_as(
            "SELECT id, expires_at FROM mail_challenges
             WHERE reference_id = ? AND recipient_email = ?
               AND challenge_kind = 'EMAIL_VERIFICATION'
             ORDER BY created_at DESC, id DESC LIMIT 1",
        )
        .bind(user_id)
        .bind(recipient_email)
        .fetch_one(database.pool())
        .await?;
        let signer = MailChallengeSigner::load_or_create(config_dir).await?;
        let token = signer
            .token_for(
                &challenge_id,
                MailChallengeKind::EmailVerification,
                expires_at,
            )
            .ok_or("email change token could not be derived")?;
        Ok((token, expires_at))
    }

    #[tokio::test]
    async fn account_email_changes_only_after_its_challenge_is_verified()
    -> Result<(), Box<dyn Error>> {
        let (directory, database) = test_database().await?;
        let config_dir = directory.path().join("config");
        add_user(&database, "email-owner", "old@example.test").await?;
        add_user(&database, "email-other", "other@example.test").await?;
        let limiter = DevicePairingRateLimiter::default();

        let requested = request_user_email_change(
            &database,
            &config_dir,
            &limiter,
            "email-owner",
            " New@Example.Test ",
            1_000,
        )
        .await?;
        assert_eq!(requested.pending_email, "new@example.test");
        let pending = pending_user_email_change(&database, "email-owner", 1_001)
            .await?
            .ok_or("pending email change was not recoverable")?;
        assert_eq!(pending.pending_email, "new@example.test");
        let user_before: (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT email, email_verified_at FROM users WHERE id = 'email-owner'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(
            user_before,
            (Some("old@example.test".to_owned()), Some(999))
        );

        let (token, _) =
            challenge_token(&database, &config_dir, "email-owner", "new@example.test").await?;
        assert_eq!(
            verify_user_email_change(&database, &config_dir, "email-other", &token, 1_010,).await,
            Err(UserEmailVerificationError::NotAcceptable)
        );
        verify_user_email_change(&database, &config_dir, "email-owner", &token, 1_010).await?;
        assert_eq!(
            verify_user_email_change(&database, &config_dir, "email-owner", &token, 1_011,).await,
            Err(UserEmailVerificationError::NotAcceptable)
        );
        let user_after: (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT email, email_verified_at FROM users WHERE id = 'email-owner'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(
            user_after,
            (Some("new@example.test".to_owned()), Some(1_010))
        );
        assert!(
            pending_user_email_change(&database, "email-owner", 1_011)
                .await?
                .is_none()
        );

        let conflict = request_user_email_change(
            &database,
            &config_dir,
            &limiter,
            "email-owner",
            "other@example.test",
            1_020,
        )
        .await?;
        let (conflict_token, _) = challenge_token(
            &database,
            &config_dir,
            "email-owner",
            &conflict.pending_email,
        )
        .await?;
        assert_eq!(
            verify_user_email_change(
                &database,
                &config_dir,
                "email-owner",
                &conflict_token,
                1_030,
            )
            .await,
            Err(UserEmailVerificationError::NotAcceptable)
        );
        let still_current: (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT email, email_verified_at FROM users WHERE id = 'email-owner'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(still_current, user_after);
        let conflict_challenge = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
            "SELECT consumed_at, revoked_at FROM mail_challenges
             WHERE reference_id = 'email-owner' AND recipient_email = 'other@example.test'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(conflict_challenge, (None, None));

        let expired = request_user_email_change(
            &database,
            &config_dir,
            &limiter,
            "email-owner",
            "expired@example.test",
            1_040,
        )
        .await?;
        let (expired_token, expires_at) = challenge_token(
            &database,
            &config_dir,
            "email-owner",
            &expired.pending_email,
        )
        .await?;
        assert_eq!(
            verify_user_email_change(
                &database,
                &config_dir,
                "email-owner",
                &expired_token,
                expires_at,
            )
            .await,
            Err(UserEmailVerificationError::NotAcceptable)
        );
        let expired_challenge: (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT consumed_at, revoked_at FROM mail_challenges
             WHERE reference_id = 'email-owner' AND recipient_email = 'expired@example.test'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(expired_challenge, (None, None));
        Ok(())
    }

    #[tokio::test]
    async fn account_and_recipient_email_rate_limits_are_enforced() -> Result<(), Box<dyn Error>> {
        let (directory, database) = test_database().await?;
        let config_dir = directory.path().join("config");
        let limiter = DevicePairingRateLimiter::default();
        add_user(&database, "account-limited", "account@example.test").await?;
        for index in 0..5 {
            request_user_email_change(
                &database,
                &config_dir,
                &limiter,
                "account-limited",
                &format!("account-{index}@example.test"),
                1_000 + index,
            )
            .await?;
        }
        assert_eq!(
            request_user_email_change(
                &database,
                &config_dir,
                &limiter,
                "account-limited",
                "account-next@example.test",
                1_010,
            )
            .await,
            Err(UserEmailVerificationError::RateLimited)
        );

        let shared_limiter = DevicePairingRateLimiter::default();
        for index in 0..6 {
            let user_id = format!("shared-email-{index}");
            add_user(&database, &user_id, &format!("{user_id}@example.test")).await?;
            let result = request_user_email_change(
                &database,
                &config_dir,
                &shared_limiter,
                &user_id,
                "shared@example.test",
                2_000 + index,
            )
            .await;
            if index < 5 {
                result?;
            } else {
                assert_eq!(result, Err(UserEmailVerificationError::RateLimited));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn mail_queue_failure_keeps_the_original_email_and_rolls_back_the_challenge()
    -> Result<(), Box<dyn Error>> {
        let (directory, database) = test_database().await?;
        let config_dir = directory.path().join("config");
        add_user(&database, "email-queue-failure", "old@example.test").await?;
        sqlx::query(
            "CREATE TRIGGER reject_email_verification_mail BEFORE INSERT ON mail_outbox
             WHEN NEW.template_kind = 'EMAIL_VERIFICATION'
             BEGIN SELECT RAISE(ABORT, 'mail queue unavailable'); END",
        )
        .execute(database.pool())
        .await?;

        assert_eq!(
            request_user_email_change(
                &database,
                &config_dir,
                &DevicePairingRateLimiter::default(),
                "email-queue-failure",
                "new@example.test",
                1_000,
            )
            .await,
            Err(UserEmailVerificationError::StorageUnavailable)
        );
        let current_email: (Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT email, email_verified_at FROM users WHERE id = 'email-queue-failure'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(
            current_email,
            (Some("old@example.test".to_owned()), Some(999))
        );
        let challenge_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM mail_challenges
             WHERE reference_id = 'email-queue-failure'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(challenge_count, 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mail_outbox")
                .fetch_one(database.pool())
                .await?,
            0
        );
        Ok(())
    }
}
