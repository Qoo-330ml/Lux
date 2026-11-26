use std::{
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::Instant,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::{fs, sync::Mutex};

use crate::{
    auth::{cache::CACHE_TTL, users::AuthenticationPrincipal},
    storage::{AuthCacheEpochs, Database},
};

pub const ADMIN_API_KEY_FILE: &str = "lux_admin_api_key";

#[derive(Clone, Copy)]
struct CachedAdminApiKey {
    epoch: u64,
    stored_at: Instant,
    key_hash: Option<[u8; 32]>,
}

#[derive(Default)]
struct AdminApiKeyCacheState {
    cached: Option<CachedAdminApiKey>,
    #[cfg(test)]
    disk_reads: usize,
}

#[derive(Clone)]
pub struct AdminApiKeyService {
    config_dir: PathBuf,
    write_lock: Arc<Mutex<()>>,
    auth_cache: Arc<AuthCacheEpochs>,
    cache: Arc<StdMutex<AdminApiKeyCacheState>>,
}

impl AdminApiKeyService {
    pub fn new(config_dir: PathBuf, database: Database) -> Self {
        Self {
            config_dir,
            write_lock: Arc::new(Mutex::new(())),
            auth_cache: database.auth_cache_epochs(),
            cache: Arc::new(StdMutex::new(AdminApiKeyCacheState::default())),
        }
    }

    pub async fn current(&self) -> Result<Option<String>, AdminApiKeyError> {
        read_key(&self.key_path()).await
    }

    pub async fn rotate(&self) -> Result<String, AdminApiKeyError> {
        let _guard = self.write_lock.lock().await;
        let key = generate_key()?;
        write_key(&self.key_path(), Some(&key)).await?;
        self.invalidate_cache();
        Ok(key)
    }

    pub async fn revoke(&self) -> Result<(), AdminApiKeyError> {
        let _guard = self.write_lock.lock().await;
        write_key(&self.key_path(), None).await?;
        self.invalidate_cache();
        Ok(())
    }

    pub async fn resolve_principal(
        &self,
        candidate: &str,
    ) -> Result<Option<AuthenticationPrincipal>, AdminApiKeyError> {
        let Some(stored_hash) = self.cached_key_hash_at(Instant::now()).await? else {
            return Ok(None);
        };
        if hash_matches(candidate.trim(), &stored_hash) {
            Ok(Some(AuthenticationPrincipal::SharedAdminApiKey))
        } else {
            Ok(None)
        }
    }

    async fn cached_key_hash_at(&self, now: Instant) -> Result<Option<[u8; 32]>, AdminApiKeyError> {
        let current_epoch = self.auth_cache.admin_api_key_epoch();
        {
            let state = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = state.cached
                && cached.epoch == current_epoch
                && now.saturating_duration_since(cached.stored_at) < CACHE_TTL
            {
                return Ok(cached.key_hash);
            }
        }
        // Snapshot the epoch before reading disk so a concurrent rotate/revoke cannot be
        // overwritten by a stale read.
        let snapshot_epoch = self.auth_cache.admin_api_key_epoch();
        #[cfg(test)]
        {
            let mut state = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.disk_reads = state.disk_reads.saturating_add(1);
        }
        let key_hash = read_key(&self.key_path())
            .await?
            .map(|stored| key_digest(&stored));
        if self.auth_cache.admin_api_key_epoch() == snapshot_epoch {
            let mut state = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.cached = Some(CachedAdminApiKey {
                epoch: snapshot_epoch,
                stored_at: now,
                key_hash,
            });
        }
        Ok(key_hash)
    }

    fn invalidate_cache(&self) {
        self.auth_cache.invalidate_admin_api_key();
        let mut state = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.cached = None;
    }

    #[cfg(test)]
    fn disk_read_count(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .disk_reads
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

fn key_digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn hash_matches(candidate: &str, stored_hash: &[u8; 32]) -> bool {
    let candidate_hash = key_digest(candidate);
    candidate_hash.ct_eq(stored_hash).into()
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

#[derive(Debug)]
pub enum AdminApiKeyError {
    Io(std::io::Error),
    TokenGeneration(String),
}

impl From<std::io::Error> for AdminApiKeyError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for AdminApiKeyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "admin API key storage failed: {error}"),
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
            Self::TokenGeneration(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::{
        auth::{
            admin_api_key::{AdminApiKeyService, CACHE_TTL},
            users::AuthenticationPrincipal,
        },
        config::Config,
        storage::Database,
    };

    #[tokio::test]
    async fn resolve_principal_caches_absent_and_present_key_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let service = AdminApiKeyService::new(config.config_dir.clone(), database.clone());

        assert_eq!(service.resolve_principal("lux_missing").await?, None);
        assert_eq!(service.resolve_principal("lux_missing_2").await?, None);
        assert_eq!(
            service.disk_read_count(),
            1,
            "absent admin API key must be cached instead of reading disk on every request"
        );

        let key = service.rotate().await?;
        assert_eq!(
            service.resolve_principal(&key).await?,
            Some(AuthenticationPrincipal::SharedAdminApiKey)
        );
        assert_eq!(service.resolve_principal("lux_wrong").await?, None);
        assert_eq!(
            service.resolve_principal(&key).await?,
            Some(AuthenticationPrincipal::SharedAdminApiKey)
        );
        assert_eq!(
            service.disk_read_count(),
            2,
            "present admin API key hash must be reused across requests without re-reading disk"
        );

        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn rotate_and_revoke_from_another_instance_invalidate_cache_immediately()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let primary = AdminApiKeyService::new(config.config_dir.clone(), database.clone());
        let secondary = AdminApiKeyService::new(config.config_dir.clone(), database.clone());

        // Populate negative cache on primary.
        assert_eq!(primary.resolve_principal("lux_none").await?, None);
        assert_eq!(primary.disk_read_count(), 1);

        // Rotating through another instance sharing the same Database must invalidate primary.
        let key = secondary.rotate().await?;
        assert_eq!(
            primary.resolve_principal(&key).await?,
            Some(AuthenticationPrincipal::SharedAdminApiKey)
        );
        assert_eq!(primary.disk_read_count(), 2);

        // Revoking through another instance must immediately reject the key on primary.
        secondary.revoke().await?;
        assert_eq!(primary.resolve_principal(&key).await?, None);
        assert_eq!(primary.disk_read_count(), 3);

        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn cached_key_reloads_after_ttl_expiry() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let service = AdminApiKeyService::new(config.config_dir.clone(), database.clone());

        let start = Instant::now();
        assert!(service.cached_key_hash_at(start).await?.is_none());
        assert_eq!(service.disk_read_count(), 1);

        // Simulate an out-of-band file write directly on disk.
        tokio::fs::create_dir_all(&config.config_dir).await?;
        tokio::fs::write(
            config.config_dir.join(super::ADMIN_API_KEY_FILE),
            "lux_external_key\n",
        )
        .await?;

        // Before TTL expiry, cache serves previous snapshot; at TTL boundary it reloads from disk.
        assert!(service.cached_key_hash_at(start).await?.is_none());
        assert_eq!(service.disk_read_count(), 1);
        assert!(
            service
                .cached_key_hash_at(start + CACHE_TTL)
                .await?
                .is_some()
        );
        assert_eq!(service.disk_read_count(), 2);

        database.close().await;
        Ok(())
    }
}
