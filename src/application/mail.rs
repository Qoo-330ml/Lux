// This foundational module is consumed by the admin settings task that follows.
#![allow(dead_code)]

use std::{
    io,
    path::{Path, PathBuf},
};

use lettre::{
    Address, AsyncSmtpTransport, Tokio1Executor, transport::smtp::authentication::Credentials,
};
use serde::{Deserialize, Serialize};
use tokio::fs;
use url::Url;

const SMTP_CREDENTIALS_FILE: &str = "smtp_credentials.json";
const SMTP_SETTINGS_FILE: &str = "smtp_settings.json";
const MAX_SMTP_HOST_LENGTH: usize = 253;
const MAX_SMTP_USERNAME_LENGTH: usize = 320;
const MAX_SENDER_NAME_LENGTH: usize = 128;
const MAX_SMTP_PASSWORD_LENGTH: usize = 4_096;

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
    fn validate(&self) -> Result<(), MailError> {
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
    TransportUnavailable,
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
    .port(settings.port);

    let builder = match credentials {
        Some(credentials) => {
            builder.credentials(Credentials::new(credentials.username, credentials.password))
        }
        None => builder,
    };
    Ok(builder.build())
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
        MailSettings, SmtpCredentials, SmtpTlsMode, build_smtp_transport, read_mail_settings,
        read_smtp_credentials, write_mail_settings, write_smtp_credentials,
    };

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
}
