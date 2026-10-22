use std::sync::Arc;

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use rand_core::OsRng;
use tokio::sync::Semaphore;

const DUMMY_PASSWORD: &[u8] = b"lux-invalid-user-password";
/// Argon2id is memory-hard (about 19 MiB per hash), so concurrent hashes are capped to keep a
/// login flood from pinning Tokio workers and multiplying that memory.
const MAX_CONCURRENT_PASSWORD_HASHES: usize = 2;

#[derive(Clone)]
pub struct PasswordService {
    dummy_hash: String,
    hash_slots: Arc<Semaphore>,
}

impl PasswordService {
    pub fn new() -> Result<Self, PasswordError> {
        Self::with_concurrency(MAX_CONCURRENT_PASSWORD_HASHES)
    }

    fn with_concurrency(limit: usize) -> Result<Self, PasswordError> {
        let salt = SaltString::encode_b64(b"lux-dummy-salt-v1")
            .map_err(|error| PasswordError::Hash(error.to_string()))?;
        let dummy_hash = Argon2::default()
            .hash_password(DUMMY_PASSWORD, &salt)
            .map_err(|error| PasswordError::Hash(error.to_string()))?
            .to_string();
        Ok(Self {
            dummy_hash,
            hash_slots: Arc::new(Semaphore::new(limit.max(1))),
        })
    }

    pub async fn hash_password(&self, password: &str) -> Result<String, PasswordError> {
        if password.is_empty() {
            return Err(PasswordError::Empty);
        }

        let password = password.to_owned();
        self.run_hash_worker(move || {
            let salt = SaltString::generate(&mut OsRng);
            Argon2::default()
                .hash_password(password.as_bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|error| PasswordError::Hash(error.to_string()))
        })
        .await
    }

    pub async fn verify_password(
        &self,
        stored_hash: Option<&str>,
        password: &str,
    ) -> Result<bool, PasswordError> {
        // Unknown users still verify against a valid Argon2id hash to keep the work factor
        // comparable to a known user and avoid a username-enumeration timing oracle.
        let hash = stored_hash.unwrap_or(&self.dummy_hash).to_owned();
        let password = password.to_owned();
        let verified = self
            .run_hash_worker(move || {
                let parsed_hash = PasswordHash::new(&hash)
                    .map_err(|error| PasswordError::InvalidHash(error.to_string()))?;
                Ok(Argon2::default()
                    .verify_password(password.as_bytes(), &parsed_hash)
                    .is_ok())
            })
            .await?;
        Ok(stored_hash.is_some() && verified)
    }

    /// Runs Argon2 on the blocking pool behind a shared concurrency limit.
    async fn run_hash_worker<T, F>(&self, work: F) -> Result<T, PasswordError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, PasswordError> + Send + 'static,
    {
        let permit = self
            .hash_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PasswordError::Hash("password worker is unavailable".to_owned()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| PasswordError::Hash(format!("password worker failed: {error}")))?
    }
}

#[derive(Debug)]
pub enum PasswordError {
    Empty,
    Hash(String),
    InvalidHash(String),
}

impl std::fmt::Display for PasswordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("password must not be empty"),
            Self::Hash(error) => write!(formatter, "password hashing failed: {error}"),
            Self::InvalidHash(error) => {
                write!(formatter, "stored password hash is invalid: {error}")
            }
        }
    }
}

impl std::error::Error for PasswordError {}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::PasswordService;

    #[tokio::test(flavor = "current_thread")]
    async fn verification_does_not_block_the_async_runtime() {
        let service = PasswordService::new().expect("password service");
        let hash = service
            .hash_password("correct horse battery staple")
            .await
            .expect("hash");

        let verified = tokio::select! {
            biased;
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                service
                    .verify_password(Some(&hash), "correct horse battery staple")
                    .await
                    .expect("verify")
            }
            verified = service.verify_password(Some(&hash), "correct horse battery staple") => {
                panic!("verification resolved before a 5ms timer: Argon2 blocked the runtime (got {verified:?})");
            }
        };
        assert!(verified);
    }

    #[tokio::test]
    async fn concurrent_hashing_shares_the_bounded_worker_slots() {
        let service = PasswordService::with_concurrency(1).expect("password service");
        let mut tasks = Vec::new();
        for index in 0..4 {
            let service = service.clone();
            tasks.push(tokio::spawn(async move {
                service.hash_password(&format!("password-{index}")).await
            }));
        }
        for task in tasks {
            assert!(task.await.expect("hashing task").is_ok());
        }
    }

    #[tokio::test]
    async fn mismatched_and_missing_hashes_never_authenticate() {
        let service = PasswordService::new().expect("password service");
        let hash = service
            .hash_password("correct horse battery staple")
            .await
            .unwrap();

        assert!(
            service
                .verify_password(Some(&hash), "correct horse battery staple")
                .await
                .unwrap()
        );
        assert!(!service.verify_password(Some(&hash), "wrong").await.unwrap());
        assert!(
            !service
                .verify_password(None, "correct horse battery staple")
                .await
                .unwrap()
        );
        assert!(matches!(
            service
                .verify_password(Some("not-a-hash"), "whatever")
                .await,
            Err(super::PasswordError::InvalidHash(_))
        ));
    }
}
