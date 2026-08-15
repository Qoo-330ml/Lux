use std::{path::PathBuf, sync::Arc};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::{fs, sync::Mutex};

use crate::{
    auth::users::UserRecord,
    domain::ids::UserId,
    storage::{Database, StorageError},
};

pub const ADMIN_API_KEY_FILE: &str = "lux_admin_api_key";

#[derive(Clone)]
pub struct AdminApiKeyService {
    config_dir: PathBuf,
    database: Database,
    write_lock: Arc<Mutex<()>>,
}

impl AdminApiKeyService {
    pub fn new(config_dir: PathBuf, database: Database) -> Self {
        Self {
            config_dir,
            database,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn current(&self) -> Result<Option<String>, AdminApiKeyError> {
        read_key(&self.key_path()).await
    }

    pub async fn rotate(&self) -> Result<String, AdminApiKeyError> {
        let _guard = self.write_lock.lock().await;
        let key = generate_key()?;
        write_key(&self.key_path(), Some(&key)).await?;
        Ok(key)
    }

    pub async fn revoke(&self) -> Result<(), AdminApiKeyError> {
        let _guard = self.write_lock.lock().await;
        write_key(&self.key_path(), None).await
    }

    pub async fn resolve(&self, candidate: &str) -> Result<Option<UserRecord>, AdminApiKeyError> {
        let Some(stored_key) = read_key(&self.key_path()).await? else {
            return Ok(None);
        };
        if !keys_match(candidate.trim(), &stored_key) {
            return Ok(None);
        }

        self.database
            .list_users()
            .await?
            .into_iter()
            .find(|user| user.can_manage_server)
            .map(user_record)
            .transpose()
            .map_err(Into::into)
    }

    fn key_path(&self) -> PathBuf {
        self.config_dir.join(ADMIN_API_KEY_FILE)
    }
}

fn generate_key() -> Result<String, AdminApiKeyError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| AdminApiKeyError::TokenGeneration(error.to_string()))?;
    Ok(format!("lux_{}", URL_SAFE_NO_PAD.encode(bytes)))
}

fn keys_match(candidate: &str, stored: &str) -> bool {
    let candidate_hash = Sha256::digest(candidate.as_bytes());
    let stored_hash = Sha256::digest(stored.as_bytes());
    candidate_hash.ct_eq(&stored_hash).into()
}

async fn read_key(path: &std::path::Path) -> Result<Option<String>, AdminApiKeyError> {
    match fs::read_to_string(path).await {
        Ok(value) => Ok((!value.trim().is_empty()).then(|| value.trim().to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AdminApiKeyError::Io(error)),
    }
}

async fn write_key(path: &std::path::Path, value: Option<&str>) -> Result<(), AdminApiKeyError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        match fs::remove_file(path).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(AdminApiKeyError::Io(error)),
        }
    };

    let path = path.to_owned();
    let temporary_path = path.with_file_name(format!(".{ADMIN_API_KEY_FILE}.tmp"));
    let contents = format!("{value}\n");
    tokio::task::spawn_blocking(move || {
        use std::{fs::OpenOptions, io::Write};

        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary_path)?;
        #[cfg(unix)]
        std::fs::set_permissions(
            &temporary_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(temporary_path, path)
    })
    .await
    .map_err(|error| AdminApiKeyError::Io(std::io::Error::other(error.to_string())))??;
    Ok(())
}

fn user_record(stored: crate::storage::StoredUser) -> Result<UserRecord, AdminApiKeyError> {
    let id: UserId = stored
        .id
        .parse()
        .map_err(|error: uuid::Error| AdminApiKeyError::InvalidUserId(error.to_string()))?;
    Ok(UserRecord {
        id,
        username_normalized: stored.username_normalized,
        display_name: stored.display_name,
        is_disabled: stored.is_disabled,
        is_admin: stored.is_admin,
        can_manage_server: stored.can_manage_server,
        can_remote_access: stored.can_remote_access,
        can_download: stored.can_download,
    })
}

#[derive(Debug)]
pub enum AdminApiKeyError {
    InvalidUserId(String),
    Io(std::io::Error),
    Storage(StorageError),
    TokenGeneration(String),
}

impl From<StorageError> for AdminApiKeyError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<std::io::Error> for AdminApiKeyError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for AdminApiKeyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUserId(error) => write!(formatter, "stored user ID is invalid: {error}"),
            Self::Io(error) => write!(formatter, "admin API key storage failed: {error}"),
            Self::Storage(error) => error.fmt(formatter),
            Self::TokenGeneration(error) => {
                write!(formatter, "admin API key generation failed: {error}")
            }
        }
    }
}

impl std::error::Error for AdminApiKeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::InvalidUserId(_) | Self::TokenGeneration(_) => None,
        }
    }
}
