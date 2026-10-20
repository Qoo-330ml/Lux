use std::{error::Error, fmt, net::IpAddr, path::Path, time::Duration};

use lettre::Address;
use uuid::Uuid;

use crate::{
    application::{
        mail::{MailChallengeSigner, hash_mail_challenge_token},
        settings::{NewUserPermissions, read_new_user_permissions, read_registration_mode},
    },
    auth::password::PasswordService,
    security::DevicePairingRateLimiter,
    storage::{
        Database, MailChallengeKind, MailOutboxTemplateKind, NewMailChallenge,
        NewMailOutboxMessage, NewUserRegistrationRequest, RegistrationMode,
        UserRegistrationCreateResult,
    },
};

const USER_REGISTRATION_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
const USER_REGISTRATION_EMAIL_RATE_LIMIT: u32 = 5;
const USER_REGISTRATION_PEER_RATE_LIMIT: u32 = 20;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum UserRegistrationError {
    InvalidEmail,
    InvalidInput,
    NotAcceptable,
    PasswordUnavailable,
    RateLimited,
    RegistrationClosed,
    SettingsUnavailable,
    SigningUnavailable,
    StorageUnavailable,
}

impl fmt::Display for UserRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEmail => formatter.write_str("registration email is invalid"),
            Self::InvalidInput => formatter.write_str("registration input is invalid"),
            Self::NotAcceptable => formatter.write_str("registration challenge is not acceptable"),
            Self::PasswordUnavailable => formatter.write_str("password hashing is unavailable"),
            Self::RateLimited => formatter.write_str("registration rate limit exceeded"),
            Self::RegistrationClosed => formatter.write_str("public registration is closed"),
            Self::SettingsUnavailable => {
                formatter.write_str("registration settings are unavailable")
            }
            Self::SigningUnavailable => formatter.write_str("registration signing is unavailable"),
            Self::StorageUnavailable => formatter.write_str("registration storage is unavailable"),
        }
    }
}

impl Error for UserRegistrationError {}

pub(crate) struct SubmitUserRegistrationInput<'a> {
    pub(crate) email: &'a str,
    pub(crate) username: &'a str,
    pub(crate) display_name: &'a str,
    pub(crate) password: &'a str,
}

pub(crate) async fn submit_public_user_registration(
    database: &Database,
    config_dir: &Path,
    rate_limiter: &DevicePairingRateLimiter,
    connection_peer: IpAddr,
    input: SubmitUserRegistrationInput<'_>,
    now: i64,
) -> Result<(), UserRegistrationError> {
    if now < 0 {
        return Err(UserRegistrationError::InvalidInput);
    }
    let recipient_email = normalize_registration_email(input.email)?;
    let username_normalized = input.username.trim().to_lowercase();
    let display_name = input.display_name.trim();
    if username_normalized.is_empty()
        || username_normalized.chars().count() > 128
        || username_normalized.chars().any(char::is_control)
        || display_name.chars().count() > 128
        || display_name.chars().any(char::is_control)
        || input.password.is_empty()
        || input.password.len() > 1_024
    {
        return Err(UserRegistrationError::InvalidInput);
    }

    let mode = read_registration_mode(database)
        .await
        .map_err(|_| UserRegistrationError::SettingsUnavailable)?;
    if mode.mode != RegistrationMode::PublicEmail {
        return Err(UserRegistrationError::RegistrationClosed);
    }

    let email_rate_key = format!("public-registration:email:{recipient_email}");
    if !rate_limiter
        .try_acquire(&email_rate_key, USER_REGISTRATION_EMAIL_RATE_LIMIT)
        .await
    {
        return Err(UserRegistrationError::RateLimited);
    }
    let peer_rate_key = format!("public-registration:peer:{connection_peer}");
    if !rate_limiter
        .try_acquire(&peer_rate_key, USER_REGISTRATION_PEER_RATE_LIMIT)
        .await
    {
        return Err(UserRegistrationError::RateLimited);
    }

    let expires_at = now
        .checked_add(USER_REGISTRATION_LIFETIME.as_secs() as i64)
        .ok_or(UserRegistrationError::InvalidInput)?;
    let mut permissions = read_new_user_permissions(database)
        .await
        .map_err(|_| UserRegistrationError::SettingsUnavailable)?;
    permissions.normalize_empty_selection();
    if !permissions.has_valid_shape() {
        permissions = NewUserPermissions::default();
    }
    let permission_snapshot_json = serde_json::to_string(&serde_json::json!({
        "isAdmin": false,
        "canManageServer": false,
        "permissions": permissions,
    }))
    .map_err(|_| UserRegistrationError::SettingsUnavailable)?;

    let password_hash = PasswordService::new()
        .and_then(|passwords| passwords.hash_password(input.password))
        .map_err(|_| UserRegistrationError::PasswordUnavailable)?;
    let user_id = Uuid::now_v7().to_string();
    let challenge_id = Uuid::now_v7().to_string();
    let signer = MailChallengeSigner::load_or_create(config_dir)
        .await
        .map_err(|_| UserRegistrationError::SigningUnavailable)?;
    let token = signer
        .token_for(
            &challenge_id,
            MailChallengeKind::EmailVerification,
            expires_at,
        )
        .ok_or(UserRegistrationError::SigningUnavailable)?;
    let token_hash = hash_mail_challenge_token(&token);
    let idempotency_key = format!("user-registration:{challenge_id}");
    let display_name = if display_name.is_empty() {
        username_normalized.as_str()
    } else {
        display_name
    };

    let result = database
        .create_user_registration_request_with_mail(
            NewUserRegistrationRequest {
                id: &user_id,
                recipient_email: &recipient_email,
                username_normalized: &username_normalized,
                display_name,
                password_hash: &password_hash,
                permission_snapshot_json: &permission_snapshot_json,
                expires_at,
            },
            NewMailChallenge {
                id: &challenge_id,
                kind: MailChallengeKind::EmailVerification,
                recipient_email: &recipient_email,
                reference_id: &user_id,
                token_hash: &token_hash,
                expires_at,
            },
            NewMailOutboxMessage {
                idempotency_key: &idempotency_key,
                recipient_email: &recipient_email,
                template_kind: MailOutboxTemplateKind::EmailVerification,
                reference_id: &challenge_id,
                available_at: now,
            },
            now,
        )
        .await
        .map_err(|_| UserRegistrationError::StorageUnavailable)?;

    match result {
        UserRegistrationCreateResult::Created
        | UserRegistrationCreateResult::AlreadyExists
        | UserRegistrationCreateResult::RateLimited => Ok(()),
    }
}

pub(crate) async fn resend_public_user_registration_email(
    database: &Database,
    rate_limiter: &DevicePairingRateLimiter,
    connection_peer: IpAddr,
    email: &str,
    idempotency_key: &str,
    now: i64,
) -> Result<(), UserRegistrationError> {
    if now < 0
        || idempotency_key.trim() != idempotency_key
        || !(1..=256).contains(&idempotency_key.len())
        || idempotency_key.chars().any(char::is_control)
    {
        return Err(UserRegistrationError::InvalidInput);
    }
    let recipient_email = normalize_registration_email(email)?;
    let mode = read_registration_mode(database)
        .await
        .map_err(|_| UserRegistrationError::SettingsUnavailable)?;
    if mode.mode != RegistrationMode::PublicEmail {
        return Err(UserRegistrationError::RegistrationClosed);
    }

    let email_rate_key = format!("public-registration:email:{recipient_email}");
    if !rate_limiter
        .try_acquire(&email_rate_key, USER_REGISTRATION_EMAIL_RATE_LIMIT)
        .await
    {
        return Err(UserRegistrationError::RateLimited);
    }
    let peer_rate_key = format!("public-registration:peer:{connection_peer}");
    if !rate_limiter
        .try_acquire(&peer_rate_key, USER_REGISTRATION_PEER_RATE_LIMIT)
        .await
    {
        return Err(UserRegistrationError::RateLimited);
    }

    database
        .queue_user_registration_resend(&recipient_email, idempotency_key, now)
        .await
        .map_err(|_| UserRegistrationError::StorageUnavailable)?;
    Ok(())
}

fn normalize_registration_email(value: &str) -> Result<String, UserRegistrationError> {
    if value.chars().any(char::is_control) {
        return Err(UserRegistrationError::InvalidEmail);
    }
    let normalized = value.trim().to_lowercase();
    if !(3..=254).contains(&normalized.len()) || normalized.parse::<Address>().is_err() {
        return Err(UserRegistrationError::InvalidEmail);
    }
    Ok(normalized)
}

pub(crate) async fn verify_public_user_registration_email(
    database: &Database,
    config_dir: &Path,
    rate_limiter: &DevicePairingRateLimiter,
    connection_peer: IpAddr,
    token: &str,
    now: i64,
) -> Result<(), UserRegistrationError> {
    if now < 0 {
        return Err(UserRegistrationError::NotAcceptable);
    }
    let peer_rate_key = format!("public-registration:peer:{connection_peer}");
    if !rate_limiter
        .try_acquire(&peer_rate_key, USER_REGISTRATION_PEER_RATE_LIMIT)
        .await
    {
        return Err(UserRegistrationError::RateLimited);
    }
    if token.trim() != token || token.len() != 43 || token.chars().any(char::is_control) {
        return Err(UserRegistrationError::NotAcceptable);
    }
    let token_hash = hash_mail_challenge_token(token);
    let challenge = database
        .get_user_registration_challenge_by_token_hash(&token_hash, now)
        .await
        .map_err(|_| UserRegistrationError::StorageUnavailable)?
        .ok_or(UserRegistrationError::NotAcceptable)?;
    let signer = MailChallengeSigner::load_or_create(config_dir)
        .await
        .map_err(|_| UserRegistrationError::SigningUnavailable)?;
    if !signer.verify(
        &challenge.id,
        MailChallengeKind::EmailVerification,
        challenge.expires_at,
        token,
    ) {
        return Err(UserRegistrationError::NotAcceptable);
    }
    match database
        .verify_user_registration_email(&challenge.id, &token_hash, now)
        .await
        .map_err(|_| UserRegistrationError::StorageUnavailable)?
    {
        crate::storage::UserRegistrationVerifyResult::Verified => Ok(()),
        crate::storage::UserRegistrationVerifyResult::NotAcceptable => {
            Err(UserRegistrationError::NotAcceptable)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use crate::{
        application::{
            mail::MailChallengeSigner, settings::NewUserPermissions, user_registration::*,
        },
        config::Config,
        security::DevicePairingRateLimiter,
        storage::{Database, MailChallengeKind, MailOutboxTemplateKind},
    };

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        Ok((directory, database))
    }

    async fn token_for_email(
        database: &Database,
        config_dir: &std::path::Path,
        email: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let (challenge_id, expires_at): (String, i64) = sqlx::query_as(
            "SELECT c.id, c.expires_at FROM mail_challenges c
             JOIN user_registration_requests r ON r.id = c.reference_id
             WHERE r.recipient_email = ? AND c.challenge_kind = 'EMAIL_VERIFICATION'",
        )
        .bind(email)
        .fetch_one(database.pool())
        .await?;
        let signer = MailChallengeSigner::load_or_create(config_dir).await?;
        signer
            .token_for(
                &challenge_id,
                MailChallengeKind::EmailVerification,
                expires_at,
            )
            .ok_or_else(|| std::io::Error::other("could not derive test verification token").into())
    }

    async fn enable_public_registration(
        database: &Database,
    ) -> Result<(), Box<dyn std::error::Error>> {
        crate::application::settings::update_registration_mode(database, "PUBLIC_EMAIL", 0)
            .await
            .map_err(|_| "could not enable public email registration")?;
        Ok(())
    }

    async fn submit(
        database: &Database,
        directory: &tempfile::TempDir,
        limiter: &DevicePairingRateLimiter,
        peer: IpAddr,
        email: &str,
        username: &str,
        now: i64,
    ) -> Result<(), UserRegistrationError> {
        submit_public_user_registration(
            database,
            &directory.path().join("config"),
            limiter,
            peer,
            SubmitUserRegistrationInput {
                email,
                username,
                display_name: username,
                password: "long-and-unique-password",
            },
            now,
        )
        .await
    }

    #[tokio::test]
    async fn submission_stays_pending_and_snapshots_admin_defaults()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        crate::application::settings::update_registration_mode(&database, "PUBLIC_EMAIL", 0)
            .await
            .map_err(|_| "could not enable public email registration")?;
        let mut permissions = NewUserPermissions::default();
        permissions.can_download = true;
        permissions.max_simultaneous_playbacks = 4;
        let permissions_json = serde_json::to_string(&permissions)?;
        database
            .set_new_user_permissions_json(&permissions_json)
            .await?;

        submit_public_user_registration(
            &database,
            &directory.path().join("config"),
            &DevicePairingRateLimiter::default(),
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)),
            SubmitUserRegistrationInput {
                email: " User@Example.Test ",
                username: "NewViewer",
                display_name: " New Viewer ",
                password: "long-and-unique-password",
            },
            1_000,
        )
        .await?;

        let (status, password_hash, snapshot_json, email_verified_at): (
            String,
            String,
            String,
            Option<i64>,
        ) = sqlx::query_as(
            "SELECT status, password_hash, permission_snapshot_json, email_verified_at
             FROM user_registration_requests WHERE recipient_email = 'user@example.test'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(status, "PENDING_EMAIL");
        assert!(password_hash.starts_with("$argon2id$"));
        assert!(!password_hash.contains("long-and-unique-password"));
        assert_eq!(email_verified_at, None);
        let snapshot: serde_json::Value = serde_json::from_str(&snapshot_json)?;
        assert_eq!(snapshot["isAdmin"], false);
        assert_eq!(snapshot["canManageServer"], false);
        assert_eq!(snapshot["permissions"]["canDownload"], true);
        assert_eq!(snapshot["permissions"]["maxSimultaneousPlaybacks"], 4);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM users WHERE email = 'user@example.test'",
            )
            .fetch_one(database.pool())
            .await?,
            0
        );

        let messages = database.list_mail_outbox_page(None, 100, 0).await?.messages;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].recipient_email, "user@example.test");
        assert_eq!(
            messages[0].template_kind,
            MailOutboxTemplateKind::EmailVerification
        );
        let challenge = database
            .get_mail_challenge(&messages[0].reference_id)
            .await?
            .ok_or("registration email challenge was not stored")?;
        assert_eq!(challenge.kind, MailChallengeKind::EmailVerification);
        assert_eq!(challenge.token_hash.len(), 32);
        Ok(())
    }

    #[tokio::test]
    async fn verification_moves_only_to_approval_and_rejects_replay_expiry_and_revocation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        let config_dir = directory.path().join("config");
        crate::application::settings::update_registration_mode(&database, "PUBLIC_EMAIL", 0)
            .await
            .map_err(|_| "could not enable public email registration")?;
        let limiter = DevicePairingRateLimiter::default();
        let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
        for (email, username, now) in [
            ("verified@example.test", "verified-user", 1_000),
            ("expired@example.test", "expired-user", 2_000),
            ("revoked@example.test", "revoked-user", 3_000),
        ] {
            submit_public_user_registration(
                &database,
                &config_dir,
                &limiter,
                peer,
                SubmitUserRegistrationInput {
                    email,
                    username,
                    display_name: username,
                    password: "long-and-unique-password",
                },
                now,
            )
            .await?;
        }

        let verified_token =
            token_for_email(&database, &config_dir, "verified@example.test").await?;
        assert_eq!(
            verify_public_user_registration_email(
                &database,
                &config_dir,
                &limiter,
                peer,
                "invalid-token",
                1_010,
            )
            .await,
            Err(UserRegistrationError::NotAcceptable)
        );
        verify_public_user_registration_email(
            &database,
            &config_dir,
            &limiter,
            peer,
            &verified_token,
            1_010,
        )
        .await?;
        assert_eq!(
            verify_public_user_registration_email(
                &database,
                &config_dir,
                &limiter,
                peer,
                &verified_token,
                1_011,
            )
            .await,
            Err(UserRegistrationError::NotAcceptable)
        );
        let (verified_status, verified_at): (String, Option<i64>) = sqlx::query_as(
            "SELECT status, email_verified_at FROM user_registration_requests
             WHERE recipient_email = 'verified@example.test'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(verified_status, "PENDING_APPROVAL");
        assert_eq!(verified_at, Some(1_010));

        let expired_token = token_for_email(&database, &config_dir, "expired@example.test").await?;
        assert_eq!(
            verify_public_user_registration_email(
                &database,
                &config_dir,
                &limiter,
                peer,
                &expired_token,
                2_000 + USER_REGISTRATION_LIFETIME.as_secs() as i64,
            )
            .await,
            Err(UserRegistrationError::NotAcceptable)
        );
        let revoked_token = token_for_email(&database, &config_dir, "revoked@example.test").await?;
        let revoked_request_id: String = sqlx::query_scalar(
            "SELECT id FROM user_registration_requests WHERE recipient_email = 'revoked@example.test'",
        )
        .fetch_one(database.pool())
        .await?;
        database
            .revoke_mail_challenges(
                MailChallengeKind::EmailVerification,
                &revoked_request_id,
                3_010,
            )
            .await?;
        assert_eq!(
            verify_public_user_registration_email(
                &database,
                &config_dir,
                &limiter,
                peer,
                &revoked_token,
                3_011,
            )
            .await,
            Err(UserRegistrationError::NotAcceptable)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM users WHERE email IN (
                    'verified@example.test', 'expired@example.test', 'revoked@example.test'
                 )",
            )
            .fetch_one(database.pool())
            .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn closed_mode_rejects_submission_and_duplicate_email_is_indistinguishable()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        let limiter = DevicePairingRateLimiter::default();
        let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));
        assert_eq!(
            submit(
                &database,
                &directory,
                &limiter,
                peer,
                "closed@example.test",
                "closed-user",
                1_000,
            )
            .await,
            Err(UserRegistrationError::RegistrationClosed)
        );
        assert_eq!(
            submit(
                &database,
                &directory,
                &limiter,
                peer,
                "not an email",
                "invalid-user",
                1_000,
            )
            .await,
            Err(UserRegistrationError::InvalidEmail)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM user_registration_requests")
                .fetch_one(database.pool())
                .await?,
            0
        );

        enable_public_registration(&database).await?;
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash, email)
             VALUES ('registered-user', 'registered-user', 'Registered User', '$argon2id$test',
                     'registered@example.test')",
        )
        .execute(database.pool())
        .await?;
        submit(
            &database,
            &directory,
            &limiter,
            peer,
            "registered@example.test",
            "another-user",
            1_001,
        )
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM user_registration_requests
                 WHERE recipient_email = 'registered@example.test'",
            )
            .fetch_one(database.pool())
            .await?,
            0
        );
        submit(
            &database,
            &directory,
            &limiter,
            peer,
            " Person@Example.Test ",
            "first-user",
            1_002,
        )
        .await?;
        submit(
            &database,
            &directory,
            &limiter,
            peer,
            "person@example.test",
            "second-user",
            1_003,
        )
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM user_registration_requests
                 WHERE recipient_email = 'person@example.test'",
            )
            .fetch_one(database.pool())
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mail_outbox
                 WHERE recipient_email = 'person@example.test'",
            )
            .fetch_one(database.pool())
            .await?,
            1
        );
        database.pool().close().await;
        assert_eq!(
            submit(
                &database,
                &directory,
                &limiter,
                peer,
                "storage-unavailable@example.test",
                "unavailable-user",
                1_004,
            )
            .await,
            Err(UserRegistrationError::SettingsUnavailable)
        );
        Ok(())
    }

    #[tokio::test]
    async fn submission_rate_limits_each_email_and_connection_peer()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        enable_public_registration(&database).await?;
        let limiter = DevicePairingRateLimiter::default();
        let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11));
        for index in 0..USER_REGISTRATION_EMAIL_RATE_LIMIT {
            submit(
                &database,
                &directory,
                &limiter,
                peer,
                if index == 0 {
                    "limited@example.test"
                } else {
                    "LIMITED@example.test"
                },
                &format!("limited-user-{index}"),
                1_000 + i64::from(index),
            )
            .await?;
        }
        assert_eq!(
            submit(
                &database,
                &directory,
                &limiter,
                peer,
                "limited@example.test",
                "limited-user-next",
                1_010,
            )
            .await,
            Err(UserRegistrationError::RateLimited)
        );
        for index in 0..USER_REGISTRATION_PEER_RATE_LIMIT {
            submit(
                &database,
                &directory,
                &limiter,
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 12)),
                &format!("peer-{index}@example.test"),
                &format!("peer-user-{index}"),
                2_000 + i64::from(index),
            )
            .await?;
        }
        assert_eq!(
            submit(
                &database,
                &directory,
                &limiter,
                IpAddr::V4(Ipv4Addr::new(203, 0, 113, 12)),
                "peer-next@example.test",
                "peer-next-user",
                2_100,
            )
            .await,
            Err(UserRegistrationError::RateLimited)
        );
        Ok(())
    }

    #[tokio::test]
    async fn resend_reuses_the_original_challenge_and_hides_missing_applications()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        enable_public_registration(&database).await?;
        let limiter = DevicePairingRateLimiter::default();
        let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 20));
        submit(
            &database,
            &directory,
            &limiter,
            peer,
            "resend@example.test",
            "resend-user",
            1_000,
        )
        .await?;
        sqlx::query(
            "UPDATE mail_outbox SET status = 'SENT', sent_at = 1_000
             WHERE recipient_email = 'resend@example.test'",
        )
        .execute(database.pool())
        .await?;
        let before_challenge: (String, Vec<u8>, i64, i64) = sqlx::query_as(
            "SELECT id, token_hash, expires_at, created_at FROM mail_challenges
             WHERE recipient_email = 'resend@example.test'",
        )
        .fetch_one(database.pool())
        .await?;
        let before_request: (String, i64) = sqlx::query_as(
            "SELECT permission_snapshot_json, expires_at FROM user_registration_requests
             WHERE recipient_email = 'resend@example.test'",
        )
        .fetch_one(database.pool())
        .await?;

        resend_public_user_registration_email(
            &database,
            &limiter,
            peer,
            "resend@example.test",
            "resend-attempt-1",
            1_001,
        )
        .await?;
        resend_public_user_registration_email(
            &database,
            &limiter,
            peer,
            "resend@example.test",
            "resend-attempt-1",
            1_002,
        )
        .await?;
        resend_public_user_registration_email(
            &database,
            &limiter,
            peer,
            "unknown@example.test",
            "resend-unknown",
            1_003,
        )
        .await?;

        let after_challenge: (String, Vec<u8>, i64, i64) = sqlx::query_as(
            "SELECT id, token_hash, expires_at, created_at FROM mail_challenges
             WHERE recipient_email = 'resend@example.test'",
        )
        .fetch_one(database.pool())
        .await?;
        let after_request: (String, i64) = sqlx::query_as(
            "SELECT permission_snapshot_json, expires_at FROM user_registration_requests
             WHERE recipient_email = 'resend@example.test'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(after_challenge, before_challenge);
        assert_eq!(after_request, before_request);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mail_outbox
                 WHERE recipient_email = 'resend@example.test'
                   AND template_kind = 'EMAIL_VERIFICATION'",
            )
            .fetch_one(database.pool())
            .await?,
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mail_outbox WHERE recipient_email = 'unknown@example.test'",
            )
            .fetch_one(database.pool())
            .await?,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn resend_applies_registration_rate_limits() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        enable_public_registration(&database).await?;
        let limiter = DevicePairingRateLimiter::default();
        let peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 21));
        submit(
            &database,
            &directory,
            &limiter,
            peer,
            "rate-limited-resend@example.test",
            "rate-limited-resend-user",
            1_000,
        )
        .await?;

        for index in 0..USER_REGISTRATION_EMAIL_RATE_LIMIT - 1 {
            resend_public_user_registration_email(
                &database,
                &limiter,
                peer,
                "rate-limited-resend@example.test",
                &format!("resend-limit-{index}"),
                1_001 + i64::from(index),
            )
            .await?;
        }
        assert_eq!(
            resend_public_user_registration_email(
                &database,
                &limiter,
                peer,
                "rate-limited-resend@example.test",
                "resend-limit-overflow",
                1_010,
            )
            .await,
            Err(UserRegistrationError::RateLimited)
        );
        Ok(())
    }
}
