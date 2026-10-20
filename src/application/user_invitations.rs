use std::{error::Error, fmt, path::Path, time::Duration};

use lettre::Address;
use uuid::Uuid;

use crate::{
    application::{
        mail::{MailChallengeSigner, hash_mail_challenge_token},
        user_presets::list_user_permission_presets,
    },
    auth::password::PasswordService,
    storage::{
        Database, MailChallengeKind, MailOutboxTemplateKind, NewInvitationUserAccount,
        NewMailChallenge, NewMailOutboxMessage, NewUserInvitation, StorageError,
        StoredUserInvitation, UserInvitationAcceptResult, UserInvitationCreateResult,
        UserInvitationResendResult, UserInvitationStatus,
    },
};

const INVITATION_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug)]
pub(crate) enum UserInvitationError {
    AlreadyPending,
    InvalidEmail,
    InvalidInput,
    InvalidPreset,
    NotFound,
    NotRevocable,
    IdempotencyConflict,
    RateLimited,
    SigningUnavailable,
    PresetUnavailable,
    Storage(StorageError),
}

impl fmt::Display for UserInvitationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyPending => formatter.write_str("an invitation is already pending"),
            Self::InvalidEmail => formatter.write_str("invitation email is invalid"),
            Self::InvalidInput => formatter.write_str("invitation input is invalid"),
            Self::InvalidPreset => formatter.write_str("permission preset is unavailable"),
            Self::NotFound => formatter.write_str("invitation was not found"),
            Self::NotRevocable => formatter.write_str("invitation is not revocable"),
            Self::IdempotencyConflict => {
                formatter.write_str("invitation resend key is already used")
            }
            Self::RateLimited => formatter.write_str("invitation email rate limit exceeded"),
            Self::SigningUnavailable => formatter.write_str("invitation signing is unavailable"),
            Self::PresetUnavailable => {
                formatter.write_str("permission preset storage is unavailable")
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl Error for UserInvitationError {}

impl From<StorageError> for UserInvitationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Debug)]
pub(crate) enum UserInvitationAcceptanceError {
    InvalidInput,
    NotAcceptable,
    UsernameTaken,
    EmailAlreadyRegistered,
    SigningUnavailable,
    PasswordUnavailable,
    Storage(StorageError),
}

impl fmt::Display for UserInvitationAcceptanceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput => formatter.write_str("invitation account input is invalid"),
            Self::NotAcceptable => formatter.write_str("invitation cannot be accepted"),
            Self::UsernameTaken => formatter.write_str("username is already in use"),
            Self::EmailAlreadyRegistered => {
                formatter.write_str("invitation email is already registered")
            }
            Self::SigningUnavailable => formatter.write_str("invitation signing is unavailable"),
            Self::PasswordUnavailable => formatter.write_str("password hashing is unavailable"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl Error for UserInvitationAcceptanceError {}

impl From<StorageError> for UserInvitationAcceptanceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct AcceptUserInvitationInput<'a> {
    pub(crate) token: &'a str,
    pub(crate) username: &'a str,
    pub(crate) display_name: &'a str,
    pub(crate) password: &'a str,
}

pub(crate) struct CreateUserInvitationInput<'a> {
    pub(crate) recipient_email: &'a str,
    pub(crate) created_by_user_id: Option<&'a str>,
    pub(crate) preset_id: &'a str,
}

pub(crate) struct CreatedUserInvitation {
    pub(crate) invitation: StoredUserInvitation,
    pub(crate) invitation_url: String,
}

pub(crate) async fn accept_user_invitation(
    database: &Database,
    config_dir: &Path,
    input: AcceptUserInvitationInput<'_>,
    now: i64,
) -> Result<(), UserInvitationAcceptanceError> {
    let username_normalized = input.username.trim().to_lowercase();
    let display_name = input.display_name.trim();
    if now < 0
        || input.token.trim() != input.token
        || !(1..=128).contains(&input.token.len())
        || input.token.chars().any(char::is_control)
        || username_normalized.is_empty()
        || username_normalized.chars().count() > 128
        || username_normalized.chars().any(char::is_control)
        || display_name.chars().count() > 128
        || display_name.chars().any(char::is_control)
        || input.password.is_empty()
        || input.password.len() > 1_024
    {
        return Err(UserInvitationAcceptanceError::InvalidInput);
    }

    let token_hash = hash_mail_challenge_token(input.token);
    let Some(acceptance) = database
        .get_user_invitation_for_challenge_hash(&token_hash, now)
        .await?
    else {
        return Err(UserInvitationAcceptanceError::NotAcceptable);
    };
    let signer = MailChallengeSigner::load_or_create(config_dir)
        .await
        .map_err(|_| UserInvitationAcceptanceError::SigningUnavailable)?;
    if !signer.verify(
        &acceptance.challenge_id,
        MailChallengeKind::Invitation,
        acceptance.expires_at,
        input.token,
    ) {
        return Err(UserInvitationAcceptanceError::NotAcceptable);
    }

    let password_hash = PasswordService::new()
        .and_then(|passwords| passwords.hash_password(input.password))
        .map_err(|_| UserInvitationAcceptanceError::PasswordUnavailable)?;
    let user_id = uuid::Uuid::now_v7().to_string();
    let display_name = if display_name.is_empty() {
        username_normalized.as_str()
    } else {
        display_name
    };
    match database
        .accept_user_invitation(
            &acceptance.challenge_id,
            &token_hash,
            NewInvitationUserAccount {
                id: &user_id,
                username_normalized: &username_normalized,
                display_name,
                password_hash: &password_hash,
            },
            now,
        )
        .await?
    {
        UserInvitationAcceptResult::Accepted => Ok(()),
        UserInvitationAcceptResult::NotAcceptable => {
            Err(UserInvitationAcceptanceError::NotAcceptable)
        }
        UserInvitationAcceptResult::UsernameTaken => {
            Err(UserInvitationAcceptanceError::UsernameTaken)
        }
        UserInvitationAcceptResult::EmailAlreadyRegistered => {
            Err(UserInvitationAcceptanceError::EmailAlreadyRegistered)
        }
    }
}

pub(crate) struct UserInvitationListPage {
    pub(crate) entries: Vec<StoredUserInvitation>,
    pub(crate) page: i64,
    pub(crate) page_size: i64,
    pub(crate) total_count: i64,
}

pub(crate) async fn create_user_invitation(
    database: &Database,
    config_dir: &Path,
    input: CreateUserInvitationInput<'_>,
    now: i64,
) -> Result<CreatedUserInvitation, UserInvitationError> {
    if now < 0
        || input.preset_id.trim() != input.preset_id
        || input.preset_id.is_empty()
        || input.created_by_user_id.is_some_and(|id| id.trim() != id)
    {
        return Err(UserInvitationError::InvalidInput);
    }
    let recipient_email = normalize_email(input.recipient_email)?;
    let expires_at = now
        .checked_add(INVITATION_LIFETIME.as_secs() as i64)
        .ok_or(UserInvitationError::InvalidInput)?;
    let presets = list_user_permission_presets(database)
        .await
        .map_err(|error| match error {
            crate::application::user_presets::UserPermissionPresetError::Storage(error) => {
                UserInvitationError::Storage(error)
            }
            _ => UserInvitationError::PresetUnavailable,
        })?;
    let preset = presets
        .into_iter()
        .find(|preset| preset.id == input.preset_id)
        .ok_or(UserInvitationError::InvalidPreset)?;
    let snapshot = serde_json::json!({
        "isAdmin": preset.is_admin,
        "canManageServer": preset.can_manage_server,
        "permissions": preset.permissions,
    });
    let permission_snapshot_json =
        serde_json::to_string(&snapshot).map_err(|_| UserInvitationError::InvalidPreset)?;

    let invitation_id = Uuid::now_v7().to_string();
    let challenge_id = Uuid::now_v7().to_string();
    let signer = MailChallengeSigner::load_or_create(config_dir)
        .await
        .map_err(|_| UserInvitationError::SigningUnavailable)?;
    let token = signer
        .token_for(&challenge_id, MailChallengeKind::Invitation, expires_at)
        .ok_or(UserInvitationError::SigningUnavailable)?;
    let token_hash = hash_mail_challenge_token(&token);
    let idempotency_key = format!("invitation:{challenge_id}");
    let result = database
        .create_user_invitation_with_mail(
            NewUserInvitation {
                id: &invitation_id,
                recipient_email: &recipient_email,
                created_by_user_id: input.created_by_user_id,
                preset_id: &preset.id,
                preset_name: &preset.name,
                preset_version: i64::try_from(preset.version)
                    .map_err(|_| UserInvitationError::InvalidPreset)?,
                permission_snapshot_json: &permission_snapshot_json,
                expires_at,
            },
            NewMailChallenge {
                id: &challenge_id,
                kind: MailChallengeKind::Invitation,
                recipient_email: &recipient_email,
                reference_id: &invitation_id,
                token_hash: &token_hash,
                expires_at,
            },
            NewMailOutboxMessage {
                idempotency_key: &idempotency_key,
                recipient_email: &recipient_email,
                template_kind: MailOutboxTemplateKind::Invitation,
                reference_id: &challenge_id,
                available_at: now,
            },
            now,
        )
        .await
        .map_err(UserInvitationError::Storage)?;
    match result {
        UserInvitationCreateResult::Created => {}
        UserInvitationCreateResult::AlreadyPending => {
            return Err(UserInvitationError::AlreadyPending);
        }
        UserInvitationCreateResult::RateLimited => {
            return Err(UserInvitationError::RateLimited);
        }
    }
    let invitation = database
        .get_user_invitation(&invitation_id, now)
        .await?
        .ok_or_else(|| {
            UserInvitationError::Storage(StorageError::Serialization(
                "created invitation could not be read".to_owned(),
            ))
        })?;
    Ok(CreatedUserInvitation {
        invitation,
        invitation_url: format!("/register/invitation#token={token}"),
    })
}

pub(crate) async fn list_user_invitations(
    database: &Database,
    status: Option<UserInvitationStatus>,
    page: i64,
    page_size: i64,
    now: i64,
) -> Result<UserInvitationListPage, UserInvitationError> {
    if now < 0 {
        return Err(UserInvitationError::InvalidInput);
    }
    let page = page.max(1);
    let page_size = page_size.clamp(1, 100);
    let offset = page.saturating_sub(1).saturating_mul(page_size);
    let result = database
        .list_user_invitations_page(status, page_size, offset, now)
        .await?;
    Ok(UserInvitationListPage {
        entries: result.entries,
        page,
        page_size: result.page_size,
        total_count: result.total_count,
    })
}

pub(crate) async fn revoke_user_invitation(
    database: &Database,
    invitation_id: &str,
    now: i64,
) -> Result<(), UserInvitationError> {
    if now < 0 || invitation_id.trim() != invitation_id || invitation_id.is_empty() {
        return Err(UserInvitationError::InvalidInput);
    }
    let invitation = database
        .get_user_invitation(invitation_id, now)
        .await?
        .ok_or(UserInvitationError::NotFound)?;
    if invitation.status != UserInvitationStatus::Pending || invitation.expires_at <= now {
        return Err(UserInvitationError::NotRevocable);
    }
    if !database.revoke_user_invitation(invitation_id, now).await? {
        return Err(UserInvitationError::NotRevocable);
    }
    Ok(())
}

pub(crate) async fn queue_user_invitation_resend(
    database: &Database,
    invitation_id: &str,
    idempotency_key: &str,
    now: i64,
) -> Result<(), UserInvitationError> {
    if now < 0
        || invitation_id.trim() != invitation_id
        || !(1..=128).contains(&invitation_id.len())
        || invitation_id.chars().any(char::is_control)
        || idempotency_key.trim() != idempotency_key
        || !(1..=256).contains(&idempotency_key.len())
        || idempotency_key.chars().any(char::is_control)
    {
        return Err(UserInvitationError::InvalidInput);
    }
    let invitation = database
        .get_user_invitation(invitation_id, now)
        .await?
        .ok_or(UserInvitationError::NotFound)?;
    if invitation.status != UserInvitationStatus::Pending || invitation.expires_at <= now {
        return Err(UserInvitationError::NotRevocable);
    }
    match database
        .queue_user_invitation_resend(invitation_id, idempotency_key, now)
        .await
    {
        Ok(UserInvitationResendResult::Queued) => Ok(()),
        Ok(UserInvitationResendResult::NotResendable) => Err(UserInvitationError::NotRevocable),
        Ok(UserInvitationResendResult::RateLimited) => Err(UserInvitationError::RateLimited),
        Err(StorageError::Conflict(_)) => Err(UserInvitationError::IdempotencyConflict),
        Err(error) => Err(UserInvitationError::Storage(error)),
    }
}

fn normalize_email(value: &str) -> Result<String, UserInvitationError> {
    if value.chars().any(char::is_control) {
        return Err(UserInvitationError::InvalidEmail);
    }
    let normalized = value.trim().to_lowercase();
    if !(3..=254).contains(&normalized.len()) || normalized.parse::<Address>().is_err() {
        return Err(UserInvitationError::InvalidEmail);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, storage::MailOutboxStatus};

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        Ok((directory, database))
    }

    async fn create(
        database: &Database,
        directory: &tempfile::TempDir,
        email: &str,
        preset_id: &str,
        now: i64,
    ) -> Result<CreatedUserInvitation, UserInvitationError> {
        create_user_invitation(
            database,
            &directory.path().join("config"),
            CreateUserInvitationInput {
                recipient_email: email,
                created_by_user_id: None,
                preset_id,
            },
            now,
        )
        .await
    }

    #[tokio::test]
    async fn creation_normalizes_email_snapshots_preset_and_returns_verifiable_link()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        let preset = crate::application::user_presets::create_user_permission_preset(
            &database,
            crate::application::user_presets::NewUserPermissionPresetInput {
                name: "Invitation Snapshot".to_owned(),
                description: None,
                is_admin: false,
                can_manage_server: false,
                permissions: crate::application::settings::NewUserPermissions::default(),
            },
        )
        .await?;
        let result = create(
            &database,
            &directory,
            " Viewer@Example.Test ",
            &preset.id,
            1_000,
        )
        .await?;

        let mut changed_permissions = preset.permissions.clone();
        changed_permissions.can_download = true;
        let updated_preset = crate::application::user_presets::update_user_permission_preset(
            &database,
            &preset.id,
            preset.version,
            crate::application::user_presets::NewUserPermissionPresetInput {
                name: preset.name,
                description: preset.description,
                is_admin: preset.is_admin,
                can_manage_server: preset.can_manage_server,
                permissions: changed_permissions,
            },
        )
        .await?;
        assert!(updated_preset.permissions.can_download);

        assert_eq!(result.invitation.recipient_email, "viewer@example.test");
        assert_eq!(result.invitation.status, UserInvitationStatus::Pending);
        assert_eq!(
            result.invitation.expires_at,
            1_000 + INVITATION_LIFETIME.as_secs() as i64
        );
        let token = result
            .invitation_url
            .strip_prefix("/register/invitation#token=")
            .ok_or("invitation link did not use the registration route")?;
        let signer = MailChallengeSigner::load_or_create(&directory.path().join("config")).await?;
        let messages = database.list_mail_outbox_page(None, 100, 0).await?.messages;
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].template_kind,
            MailOutboxTemplateKind::Invitation
        );
        let challenge = database
            .get_mail_challenge(&messages[0].reference_id)
            .await?
            .ok_or("mail challenge was not stored")?;
        assert_eq!(challenge.kind, MailChallengeKind::Invitation);
        assert_eq!(challenge.reference_id, result.invitation.id);
        assert_eq!(challenge.recipient_email, result.invitation.recipient_email);
        assert_eq!(challenge.token_hash, hash_mail_challenge_token(token));
        assert!(signer.verify(&challenge.id, challenge.kind, challenge.expires_at, token));
        let snapshot: serde_json::Value =
            serde_json::from_str(&result.invitation.permission_snapshot_json)?;
        assert_eq!(snapshot["isAdmin"], false);
        assert!(snapshot["permissions"].is_object());
        assert_eq!(snapshot["permissions"]["canDownload"], false);
        assert!(!result.invitation.permission_snapshot_json.contains(token));
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_email_conflicts_and_listing_is_bounded_and_revocation_invalidates_mail()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        let first = create(
            &database,
            &directory,
            "one@example.test",
            "builtin-user",
            1_000,
        )
        .await?;
        assert!(matches!(
            create(
                &database,
                &directory,
                "ONE@example.test",
                "builtin-user",
                1_001
            )
            .await,
            Err(UserInvitationError::AlreadyPending)
        ));
        let second = create(
            &database,
            &directory,
            "two@example.test",
            "builtin-user",
            1_002,
        )
        .await?;
        let pending = list_user_invitations(
            &database,
            Some(UserInvitationStatus::Pending),
            1,
            1_000,
            1_002,
        )
        .await?;
        assert_eq!(pending.page_size, 100);
        assert_eq!(pending.total_count, 2);
        assert_eq!(pending.entries.len(), 2);
        assert_eq!(pending.entries[0].id, second.invitation.id);

        revoke_user_invitation(&database, &first.invitation.id, 1_003).await?;
        let queued_messages = database.list_mail_outbox_page(None, 100, 0).await?.messages;
        let revoked_message = queued_messages
            .iter()
            .find(|message| message.recipient_email == first.invitation.recipient_email)
            .ok_or("invitation mail was not queued")?;
        assert_eq!(revoked_message.status, MailOutboxStatus::Failed);
        let revoked_challenge = database
            .get_mail_challenge(&revoked_message.reference_id)
            .await?
            .ok_or("invitation challenge was not stored")?;
        assert_eq!(revoked_challenge.revoked_at, Some(1_003));
        assert!(matches!(
            revoke_user_invitation(&database, &first.invitation.id, 1_004).await,
            Err(UserInvitationError::NotRevocable)
        ));
        assert!(matches!(
            revoke_user_invitation(
                &database,
                &second.invitation.id,
                1_002 + INVITATION_LIFETIME.as_secs() as i64,
            )
            .await,
            Err(UserInvitationError::NotRevocable)
        ));
        let expired = list_user_invitations(
            &database,
            Some(UserInvitationStatus::Expired),
            1,
            20,
            1_002 + INVITATION_LIFETIME.as_secs() as i64,
        )
        .await?;
        assert_eq!(expired.total_count, 1);
        assert_eq!(expired.entries[0].id, second.invitation.id);
        Ok(())
    }

    #[tokio::test]
    async fn email_rate_limit_is_propagated_without_creating_an_invitation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        for index in 0..5 {
            let now = 1_000 + index * 2;
            let invitation = create(
                &database,
                &directory,
                "limited@example.test",
                "builtin-user",
                now,
            )
            .await?;
            revoke_user_invitation(&database, &invitation.invitation.id, now + 1).await?;
        }

        assert!(matches!(
            create(
                &database,
                &directory,
                "LIMITED@example.test",
                "builtin-user",
                1_010,
            )
            .await,
            Err(UserInvitationError::RateLimited)
        ));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM user_invitations
                 WHERE recipient_email = 'limited@example.test'",
            )
            .fetch_one(database.pool())
            .await?,
            5
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM mail_outbox
                 WHERE recipient_email = 'limited@example.test'",
            )
            .fetch_one(database.pool())
            .await?,
            5
        );
        Ok(())
    }

    #[tokio::test]
    async fn invalid_email_and_missing_preset_are_rejected_without_queueing()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        assert!(matches!(
            create(&database, &directory, "not-an-email", "builtin-user", 1_000).await,
            Err(UserInvitationError::InvalidEmail)
        ));
        assert!(matches!(
            create(
                &database,
                &directory,
                "valid@example.test",
                "missing",
                1_000
            )
            .await,
            Err(UserInvitationError::InvalidPreset)
        ));
        assert!(
            database
                .list_mail_outbox_page(None, 100, 0)
                .await?
                .messages
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn invitation_acceptance_verifies_token_and_creates_argon2_user_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let (directory, database) = test_database().await?;
        let invitation = create(
            &database,
            &directory,
            "accept-service@example.test",
            "builtin-user",
            2_000,
        )
        .await?;
        let token = invitation
            .invitation_url
            .strip_prefix("/register/invitation#token=")
            .ok_or("invitation token missing from fragment URL")?;
        let input = AcceptUserInvitationInput {
            token,
            username: "  Invitation User ",
            display_name: "  Invitation Display  ",
            password: "correct horse battery staple",
        };

        assert!(matches!(
            accept_user_invitation(
                &database,
                &directory.path().join("config"),
                AcceptUserInvitationInput {
                    token: "invalid-token",
                    ..input
                },
                2_001,
            )
            .await,
            Err(UserInvitationAcceptanceError::NotAcceptable)
        ));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users")
                .fetch_one(database.pool())
                .await?,
            0
        );
        let oversized_password = "x".repeat(1_025);
        assert!(matches!(
            accept_user_invitation(
                &database,
                &directory.path().join("config"),
                AcceptUserInvitationInput {
                    password: &oversized_password,
                    ..input
                },
                2_001,
            )
            .await,
            Err(UserInvitationAcceptanceError::InvalidInput)
        ));

        accept_user_invitation(&database, &directory.path().join("config"), input, 2_001).await?;

        let (email, display_name, email_verified_at, is_disabled, password_hash): (
            String,
            String,
            Option<i64>,
            i64,
            String,
        ) = sqlx::query_as(
            "SELECT email, display_name, email_verified_at, is_disabled, password_hash FROM users
             WHERE username_normalized = 'invitation user'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(email, "accept-service@example.test");
        assert_eq!(display_name, "Invitation Display");
        assert_eq!(email_verified_at, Some(2_001));
        assert_eq!(is_disabled, 0);
        assert!(!password_hash.contains("correct horse battery staple"));
        assert!(
            crate::auth::password::PasswordService::new()?
                .verify_password(Some(&password_hash), "correct horse battery staple")?
        );
        assert!(matches!(
            accept_user_invitation(&database, &directory.path().join("config"), input, 2_002,)
                .await,
            Err(UserInvitationAcceptanceError::NotAcceptable)
        ));

        let username_conflict = create(
            &database,
            &directory,
            "username-conflict@example.test",
            "builtin-user",
            3_000,
        )
        .await?;
        let username_conflict_token = username_conflict
            .invitation_url
            .strip_prefix("/register/invitation#token=")
            .ok_or("username-conflict token missing")?;
        assert!(matches!(
            accept_user_invitation(
                &database,
                &directory.path().join("config"),
                AcceptUserInvitationInput {
                    token: username_conflict_token,
                    username: "INVITATION USER",
                    display_name: "Conflict",
                    password: "another valid password",
                },
                3_001,
            )
            .await,
            Err(UserInvitationAcceptanceError::UsernameTaken)
        ));
        assert_eq!(
            database
                .get_user_invitation(&username_conflict.invitation.id, 3_001)
                .await?
                .ok_or("username-conflict invitation disappeared")?
                .status,
            UserInvitationStatus::Pending
        );

        let email_conflict = create(
            &database,
            &directory,
            "accept-service@example.test",
            "builtin-user",
            4_000,
        )
        .await?;
        let email_conflict_token = email_conflict
            .invitation_url
            .strip_prefix("/register/invitation#token=")
            .ok_or("email-conflict token missing")?;
        assert!(matches!(
            accept_user_invitation(
                &database,
                &directory.path().join("config"),
                AcceptUserInvitationInput {
                    token: email_conflict_token,
                    username: "new-account",
                    display_name: "New Account",
                    password: "another valid password",
                },
                4_001,
            )
            .await,
            Err(UserInvitationAcceptanceError::EmailAlreadyRegistered)
        ));
        assert_eq!(
            database
                .get_user_invitation(&email_conflict.invitation.id, 4_001)
                .await?
                .ok_or("email-conflict invitation disappeared")?
                .status,
            UserInvitationStatus::Pending
        );
        Ok(())
    }
}
