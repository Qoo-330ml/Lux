// This foundational module is consumed by the admin settings task that follows.
#![allow(dead_code)]

use std::{
    io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use lettre::{
    Address, AsyncSmtpTransport, Tokio1Executor, transport::smtp::authentication::Credentials,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::{fs, task::spawn_blocking};
use url::Url;

use crate::storage::MailChallengeKind;

const SMTP_CREDENTIALS_FILE: &str = "smtp_credentials.json";
const SMTP_SETTINGS_FILE: &str = "smtp_settings.json";
const MAIL_CHALLENGE_SIGNING_KEY_FILE: &str = "mail_challenge_signing_key";
const MAX_SMTP_HOST_LENGTH: usize = 253;
const MAX_SMTP_USERNAME_LENGTH: usize = 320;
const MAX_SENDER_NAME_LENGTH: usize = 128;
const MAX_SMTP_PASSWORD_LENGTH: usize = 4_096;
const SMTP_OPERATION_TIMEOUT: Duration = Duration::from_secs(8);
const MAIL_CHALLENGE_TOKEN_DOMAIN: &[u8] = b"lux-mail-challenge-token-v1\0";

type HmacSha256 = Hmac<Sha256>;

static MAIL_CHALLENGE_SIGNING_KEY_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum SmtpTlsMode {
    Smtps,
    StartTls,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MailSettings {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls_mode: SmtpTlsMode,
    pub(crate) sender_name: String,
    pub(crate) sender_email: String,
    pub(crate) site_url: String,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SmtpCredentials {
    pub(crate) username: String,
    pub(crate) password: String,
}

impl SmtpCredentials {
    pub(crate) fn validate(&self) -> Result<(), MailError> {
        if self.username.trim().is_empty()
            || self.username.trim() != self.username
            || self.username.len() > MAX_SMTP_USERNAME_LENGTH
            || self.username.chars().any(char::is_control)
            || self.password.is_empty()
            || self.password.len() > MAX_SMTP_PASSWORD_LENGTH
        {
            return Err(MailError::CredentialsRejected);
        }
        Ok(())
    }
}

impl MailSettings {
    pub(crate) fn validate(&self) -> Result<(), MailError> {
        let host = self.host.trim();
        if host.is_empty()
            || host.len() > MAX_SMTP_HOST_LENGTH
            || host != self.host
            || host.chars().any(char::is_whitespace)
        {
            return Err(MailError::SettingsRejected);
        }
        let relay_url =
            Url::parse(&format!("smtp://{host}/")).map_err(|_| MailError::SettingsRejected)?;
        if relay_url.host_str().is_none()
            || !relay_url.username().is_empty()
            || relay_url.password().is_some()
            || relay_url.port().is_some()
            || relay_url.path() != "/"
        {
            return Err(MailError::SettingsRejected);
        }
        if self.port == 0 {
            return Err(MailError::SettingsRejected);
        }
        if self.sender_name.trim().is_empty()
            || self.sender_name.len() > MAX_SENDER_NAME_LENGTH
            || self.sender_name.chars().any(char::is_control)
        {
            return Err(MailError::SettingsRejected);
        }
        if self.sender_email.len() > 320
            || self.sender_email.trim() != self.sender_email
            || self.sender_email.parse::<Address>().is_err()
        {
            return Err(MailError::SettingsRejected);
        }
        let site_url = Url::parse(&self.site_url).map_err(|_| MailError::SettingsRejected)?;
        if self.site_url.len() > 2_048
            || !matches!(site_url.scheme(), "http" | "https")
            || site_url.host_str().is_none()
            || !site_url.username().is_empty()
            || site_url.password().is_some()
            || site_url.query().is_some()
            || site_url.fragment().is_some()
        {
            return Err(MailError::SettingsRejected);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MailError {
    SettingsRejected,
    CredentialsRejected,
    SettingsUnavailable,
    CredentialsUnavailable,
    SettingsNotConfigured,
    ConnectionFailed,
    TransportUnavailable,
}

pub(crate) struct MailChallengeSigner {
    key: [u8; 32],
}

impl MailChallengeSigner {
    pub(crate) async fn load_or_create(config_dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(config_dir).await?;
        let key_path = config_dir.join(MAIL_CHALLENGE_SIGNING_KEY_FILE);
        let key = spawn_blocking(move || {
            let _guard = MAIL_CHALLENGE_SIGNING_KEY_LOCK
                .lock()
                .map_err(|_| io::Error::other("mail challenge signing key lock failed"))?;
            read_or_create_mail_challenge_signing_key(&key_path)
        })
        .await
        .map_err(|error| io::Error::other(error.to_string()))??;
        Ok(Self { key })
    }

    pub(crate) fn token_for(
        &self,
        challenge_id: &str,
        kind: MailChallengeKind,
        expires_at: i64,
    ) -> Option<String> {
        if challenge_id.trim() != challenge_id
            || !(1..=128).contains(&challenge_id.len())
            || challenge_id.chars().any(char::is_control)
            || expires_at < 0
        {
            return None;
        }
        let mut mac = HmacSha256::new_from_slice(&self.key).ok()?;
        mac.update(MAIL_CHALLENGE_TOKEN_DOMAIN);
        mac.update(&(challenge_id.len() as u64).to_be_bytes());
        mac.update(challenge_id.as_bytes());
        let kind_tag: &[u8] = match kind {
            MailChallengeKind::Invitation => b"INVITATION",
            MailChallengeKind::EmailVerification => b"EMAIL_VERIFICATION",
        };
        mac.update(&(kind_tag.len() as u64).to_be_bytes());
        mac.update(kind_tag);
        mac.update(&expires_at.to_be_bytes());
        Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }

    pub(crate) fn verify(
        &self,
        challenge_id: &str,
        kind: MailChallengeKind,
        expires_at: i64,
        supplied_token: &str,
    ) -> bool {
        if supplied_token.len() != 43 {
            return false;
        }
        let Some(expected_token) = self.token_for(challenge_id, kind, expires_at) else {
            return false;
        };
        hash_mail_challenge_token(&expected_token)
            .ct_eq(&hash_mail_challenge_token(supplied_token))
            .into()
    }
}

pub(crate) fn hash_mail_challenge_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub(crate) fn build_smtp_transport(
    settings: &MailSettings,
    credentials: Option<SmtpCredentials>,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, MailError> {
    settings.validate()?;
    if let Some(credentials) = &credentials {
        credentials.validate()?;
    }

    let builder = match settings.tls_mode {
        SmtpTlsMode::Smtps => AsyncSmtpTransport::<Tokio1Executor>::relay(&settings.host),
        SmtpTlsMode::StartTls => {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&settings.host)
        }
    }
    .map_err(|_| MailError::TransportUnavailable)?
    .port(settings.port)
    .timeout(Some(SMTP_OPERATION_TIMEOUT));

    let builder = match credentials {
        Some(credentials) => {
            builder.credentials(Credentials::new(credentials.username, credentials.password))
        }
        None => builder,
    };
    Ok(builder.build())
}

pub(crate) async fn test_smtp_connection(config_dir: &Path) -> Result<(), MailError> {
    let settings = read_mail_settings(config_dir)
        .await
        .map_err(|_| MailError::SettingsUnavailable)?
        .ok_or(MailError::SettingsNotConfigured)?;
    let credentials = read_smtp_credentials(config_dir)
        .await
        .map_err(|_| MailError::CredentialsUnavailable)?;
    let transport = build_smtp_transport(&settings, credentials)?;
    match tokio::time::timeout(SMTP_OPERATION_TIMEOUT, transport.test_connection()).await {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false) | Err(_)) | Err(_) => Err(MailError::ConnectionFailed),
    }
}

pub(crate) async fn read_smtp_credentials(
    config_dir: &Path,
) -> io::Result<Option<SmtpCredentials>> {
    let path = config_dir.join(SMTP_CREDENTIALS_FILE);
    let metadata = match fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SMTP credentials file must not be a symbolic link",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SMTP credentials file permissions must be 0600",
            ));
        }
    }
    let serialized = fs::read_to_string(path).await?;
    let credentials: SmtpCredentials = serde_json::from_str(&serialized)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SMTP credentials"))?;
    credentials
        .validate()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SMTP credentials"))?;
    Ok(Some(credentials))
}

pub(crate) async fn read_mail_settings(config_dir: &Path) -> io::Result<Option<MailSettings>> {
    let path = config_dir.join(SMTP_SETTINGS_FILE);
    let serialized = match fs::read_to_string(path).await {
        Ok(serialized) => serialized,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let settings: MailSettings = serde_json::from_str(&serialized)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    settings
        .validate()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SMTP settings"))?;
    Ok(Some(settings))
}

pub(crate) async fn write_mail_settings(
    config_dir: &Path,
    settings: Option<&MailSettings>,
) -> io::Result<()> {
    fs::create_dir_all(config_dir).await?;
    let target = config_dir.join(SMTP_SETTINGS_FILE);
    let Some(settings) = settings else {
        return match fs::remove_file(target).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    };
    settings
        .validate()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid SMTP settings"))?;
    let serialized = serde_json::to_vec(settings)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

    let temporary = config_dir.join(format!(".smtp_settings.{}.tmp", uuid::Uuid::now_v7()));
    let target_for_write = target.clone();
    let temporary_for_write = temporary.clone();
    tokio::task::spawn_blocking(move || {
        write_private_file_atomically(&target_for_write, &temporary_for_write, &serialized)
    })
    .await
    .map_err(|error| io::Error::other(error.to_string()))??;
    Ok(())
}

pub(crate) async fn write_smtp_credentials(
    config_dir: &Path,
    credentials: Option<&SmtpCredentials>,
) -> io::Result<()> {
    fs::create_dir_all(config_dir).await?;
    let target = config_dir.join(SMTP_CREDENTIALS_FILE);
    let Some(credentials) = credentials else {
        return match fs::remove_file(target).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    };
    credentials
        .validate()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid SMTP credentials"))?;
    let serialized = serde_json::to_vec(credentials)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

    let temporary = config_dir.join(format!(".smtp_credentials.{}.tmp", uuid::Uuid::now_v7()));
    let target_for_write = target.clone();
    let temporary_for_write = temporary.clone();
    tokio::task::spawn_blocking(move || {
        write_private_file_atomically(&target_for_write, &temporary_for_write, &serialized)
    })
    .await
    .map_err(|error| io::Error::other(error.to_string()))??;
    Ok(())
}

fn read_or_create_mail_challenge_signing_key(path: &Path) -> io::Result<[u8; 32]> {
    match read_private_mail_challenge_signing_key(path) {
        Ok(key) => return Ok(key),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut generated = [0_u8; 32];
    OsRng.fill_bytes(&mut generated);
    if create_private_file_if_absent(path, &generated)? {
        Ok(generated)
    } else {
        read_private_mail_challenge_signing_key(path)
    }
}

fn read_private_mail_challenge_signing_key(path: &Path) -> io::Result<[u8; 32]> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "mail challenge signing key must be a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mail challenge signing key permissions must be 0600",
            ));
        }
    }
    let bytes = std::fs::read(path)?;
    bytes.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "mail challenge signing key has invalid length",
        )
    })
}

fn create_private_file_if_absent(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing key directory"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid key file name"))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::now_v7()));
    let install_result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match std::fs::hard_link(&temporary, path) {
            Ok(()) => {
                #[cfg(unix)]
                std::fs::File::open(parent)?.sync_all()?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    })();
    if let Err(error) = std::fs::remove_file(&temporary)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error);
    }
    install_result
}

fn write_private_file_atomically(
    target: &PathBuf,
    temporary: &PathBuf,
    bytes: &[u8],
) -> io::Result<()> {
    use std::io::Write;

    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(temporary, target)?;
        if let Some(parent) = target.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::{
        MailChallengeSigner, MailSettings, SmtpCredentials, SmtpTlsMode, build_smtp_transport,
        hash_mail_challenge_token, read_mail_settings, read_smtp_credentials, write_mail_settings,
        write_smtp_credentials,
    };
    use crate::storage::MailChallengeKind;

    fn valid_settings() -> MailSettings {
        MailSettings {
            host: "smtp.example.test".to_owned(),
            port: 465,
            tls_mode: SmtpTlsMode::Smtps,
            sender_name: "Lux".to_owned(),
            sender_email: "noreply@example.test".to_owned(),
            site_url: "https://lux.example.test".to_owned(),
        }
    }

    #[tokio::test]
    async fn smtp_credentials_are_stored_separately_with_private_permissions()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let credentials_path = directory.path().join("smtp_credentials.json");
        let credentials = SmtpCredentials {
            username: "lux@example.test".to_owned(),
            password: "secret-value".to_owned(),
        };

        write_smtp_credentials(directory.path(), Some(&credentials)).await?;
        let loaded = read_smtp_credentials(directory.path()).await?;
        assert!(loaded.as_ref().is_some_and(|loaded| {
            loaded.username == "lux@example.test" && loaded.password == "secret-value"
        }));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&credentials_path)?.permissions().mode() & 0o777,
                0o600
            );

            std::fs::set_permissions(&credentials_path, std::fs::Permissions::from_mode(0o640))?;
            assert!(
                read_smtp_credentials(directory.path())
                    .await
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
            );
        }

        write_smtp_credentials(directory.path(), Some(&credentials)).await?;
        assert!(read_smtp_credentials(directory.path()).await?.is_some_and(
            |loaded| loaded.username == "lux@example.test" && loaded.password == "secret-value"
        ));

        write_smtp_credentials(directory.path(), None).await?;
        assert!(read_smtp_credentials(directory.path()).await?.is_none());
        assert!(!Path::new(&credentials_path).exists());
        #[cfg(unix)]
        {
            let external_secret = directory.path().join("external-credentials");
            tokio::fs::write(
                &external_secret,
                r#"{"username":"attacker","password":"do-not-read"}"#,
            )
            .await?;
            std::os::unix::fs::symlink(&external_secret, &credentials_path)?;
            assert!(
                read_smtp_credentials(directory.path())
                    .await
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
            );
            write_smtp_credentials(directory.path(), None).await?;
            assert!(
                tokio::fs::read_to_string(external_secret)
                    .await?
                    .contains("do-not-read")
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn mail_settings_round_trip_without_smtp_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let settings = valid_settings();
        write_mail_settings(directory.path(), Some(&settings)).await?;

        assert_eq!(read_mail_settings(directory.path()).await?, Some(settings));
        let serialized =
            tokio::fs::read_to_string(directory.path().join("smtp_settings.json")).await?;
        assert!(!serialized.to_ascii_lowercase().contains("password"));

        write_mail_settings(directory.path(), None).await?;
        assert_eq!(read_mail_settings(directory.path()).await?, None);
        Ok(())
    }

    #[test]
    fn settings_validate_mail_headers_and_site_url() {
        assert!(valid_settings().validate().is_ok());

        let mut settings = valid_settings();
        settings.sender_email = "not-an-email".to_owned();
        assert!(settings.validate().is_err());

        let mut settings = valid_settings();
        settings.sender_name = "Lux\r\nBcc: attacker@example.test".to_owned();
        assert!(settings.validate().is_err());

        let mut settings = valid_settings();
        settings.sender_name = "Lux\tHeader".to_owned();
        assert!(settings.validate().is_err());

        let mut settings = valid_settings();
        settings.site_url = "javascript:alert(1)".to_owned();
        assert!(settings.validate().is_err());

        let mut settings = valid_settings();
        settings.host = "smtp.example.test:587".to_owned();
        assert!(settings.validate().is_err());

        let mut settings = valid_settings();
        settings.host.clear();
        assert!(settings.validate().is_err());

        let mut settings = valid_settings();
        settings.host = "a".repeat(254);
        assert!(settings.validate().is_err());

        let mut settings = valid_settings();
        settings.port = 0;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn mail_settings_never_serialize_smtp_credentials() -> Result<(), Box<dyn std::error::Error>> {
        let serialized = serde_json::to_string(&valid_settings())?;
        assert!(!serialized.to_ascii_lowercase().contains("password"));
        assert!(!serialized.to_ascii_lowercase().contains("username"));
        Ok(())
    }

    #[test]
    fn tls_mode_rejects_plaintext_values() -> Result<(), Box<dyn std::error::Error>> {
        let mut serialized = serde_json::to_value(valid_settings())?;
        serialized["tlsMode"] = serde_json::Value::String("PLAINTEXT".to_owned());
        assert!(serde_json::from_value::<MailSettings>(serialized).is_err());
        Ok(())
    }

    #[test]
    fn tls_transport_is_constructed_without_opening_a_connection() {
        let credentials = SmtpCredentials {
            username: "lux@example.test".to_owned(),
            password: "secret-value".to_owned(),
        };
        assert!(build_smtp_transport(&valid_settings(), Some(credentials.clone())).is_ok());
        assert!(build_smtp_transport(&valid_settings(), None).is_ok());
        assert!(
            build_smtp_transport(
                &valid_settings(),
                Some(SmtpCredentials {
                    username: " ".to_owned(),
                    password: "secret-value".to_owned(),
                })
            )
            .is_err()
        );

        let mut settings = valid_settings();
        settings.tls_mode = SmtpTlsMode::StartTls;
        settings.port = 587;
        assert!(build_smtp_transport(&settings, Some(credentials)).is_ok());
    }

    #[tokio::test]
    async fn mail_challenge_token_is_stable_scoped_and_hashable()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let signer = MailChallengeSigner::load_or_create(directory.path())
            .await
            .map_err(|_| "mail challenge signer could not be loaded")?;
        let token = signer
            .token_for("challenge-1", MailChallengeKind::Invitation, 1234)
            .ok_or("could not derive invitation token")?;
        let verification_token = signer
            .token_for("challenge-1", MailChallengeKind::EmailVerification, 1234)
            .ok_or("could not derive verification token")?;
        assert_ne!(token, verification_token);
        assert_ne!(
            token,
            signer
                .token_for("challenge-2", MailChallengeKind::Invitation, 1234)
                .ok_or("could not derive second token")?
        );
        assert_ne!(
            token,
            signer
                .token_for("challenge-1", MailChallengeKind::Invitation, 1235)
                .ok_or("could not derive renewed token")?
        );
        assert!(signer.verify("challenge-1", MailChallengeKind::Invitation, 1234, &token));
        assert!(!signer.verify(
            "challenge-1",
            MailChallengeKind::Invitation,
            1234,
            &verification_token
        ));
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
        let digest = hash_mail_challenge_token(&token);
        assert_eq!(digest.len(), 32);
        assert_ne!(digest.as_slice(), token.as_bytes());

        let loaded_again = MailChallengeSigner::load_or_create(directory.path())
            .await
            .map_err(|_| "mail challenge signer could not be reloaded")?;
        assert_eq!(
            token,
            loaded_again
                .token_for("challenge-1", MailChallengeKind::Invitation, 1234)
                .ok_or("could not derive reloaded token")?
        );
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_mail_challenge_signer_initialization_uses_one_key()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let mut loads = Vec::new();
        for _ in 0..8 {
            let config_dir = directory.path().to_owned();
            loads.push(tokio::spawn(async move {
                MailChallengeSigner::load_or_create(&config_dir).await
            }));
        }
        let mut tokens = Vec::new();
        for load in loads {
            let signer = load
                .await?
                .map_err(|_| "mail challenge signer could not be loaded")?;
            tokens.push(
                signer
                    .token_for("challenge-1", MailChallengeKind::Invitation, 1234)
                    .ok_or("could not derive concurrent token")?,
            );
        }
        assert!(tokens.windows(2).all(|pair| pair[0] == pair[1]));
        Ok(())
    }

    #[tokio::test]
    async fn mail_challenge_signing_key_is_private_and_rejects_invalid_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let key_path = directory.path().join("mail_challenge_signing_key");
        MailChallengeSigner::load_or_create(directory.path())
            .await
            .map_err(|_| "mail challenge signer could not be loaded")?;
        let key = tokio::fs::read(&key_path).await?;
        assert_eq!(key.len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&key_path)?.permissions().mode() & 0o777,
                0o600
            );
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o640))?;
            assert!(
                MailChallengeSigner::load_or_create(directory.path())
                    .await
                    .is_err()
            );
            std::fs::remove_file(&key_path)?;
            let external_key = directory.path().join("external-key");
            tokio::fs::write(&external_key, [5_u8; 32]).await?;
            std::os::unix::fs::symlink(&external_key, &key_path)?;
            assert!(
                MailChallengeSigner::load_or_create(directory.path())
                    .await
                    .is_err()
            );
        }
        std::fs::remove_file(&key_path)?;
        tokio::fs::write(&key_path, b"short").await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
        }
        assert!(
            MailChallengeSigner::load_or_create(directory.path())
                .await
                .is_err_and(|error| error.kind() == std::io::ErrorKind::InvalidData)
        );
        Ok(())
    }
}
