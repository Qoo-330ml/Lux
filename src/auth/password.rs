use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use rand_core::OsRng;

const DUMMY_PASSWORD: &[u8] = b"lux-invalid-user-password";

#[derive(Clone)]
pub struct PasswordService {
    dummy_hash: String,
}

impl PasswordService {
    pub fn new() -> Result<Self, PasswordError> {
        let salt = SaltString::encode_b64(b"lux-dummy-salt-v1")
            .map_err(|error| PasswordError::Hash(error.to_string()))?;
        let dummy_hash = Argon2::default()
            .hash_password(DUMMY_PASSWORD, &salt)
            .map_err(|error| PasswordError::Hash(error.to_string()))?
            .to_string();
        Ok(Self { dummy_hash })
    }

    pub fn hash_password(&self, password: &str) -> Result<String, PasswordError> {
        if password.is_empty() {
            return Err(PasswordError::Empty);
        }

        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|error| PasswordError::Hash(error.to_string()))
    }

    pub fn verify_password(
        &self,
        stored_hash: Option<&str>,
        password: &str,
    ) -> Result<bool, PasswordError> {
        // Unknown users still verify against a valid Argon2id hash to keep the work factor
        // comparable to a known user and avoid a username-enumeration timing oracle.
        let hash = stored_hash.unwrap_or(&self.dummy_hash);
        let parsed_hash = PasswordHash::new(hash)
            .map_err(|error| PasswordError::InvalidHash(error.to_string()))?;
        let verified = Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok();
        Ok(stored_hash.is_some() && verified)
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
