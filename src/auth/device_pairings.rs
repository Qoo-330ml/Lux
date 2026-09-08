use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::storage::{
    Database, DevicePairingRedeemResult, NewDeviceAccessToken, NewDevicePairing, StorageError,
};

pub const DEVICE_PAIRING_LIFETIME_SECONDS: i64 = 5 * 60;
pub const DEVICE_PAIRING_CLIENT_NAME: &str = "Lux Prism";

#[derive(Clone)]
pub struct DevicePairingService {
    database: Database,
}

impl DevicePairingService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn create(&self, user_id: &str) -> Result<DevicePairingCreated, DevicePairingError> {
        let secret = generate_random_value()?;
        let secret_hash = hash_value(&secret);
        let pairing_id = Uuid::now_v7().to_string();
        let expires_at = current_unix_timestamp().saturating_add(DEVICE_PAIRING_LIFETIME_SECONDS);
        self.database
            .create_device_pairing(NewDevicePairing {
                id: &pairing_id,
                user_id,
                secret_hash: &secret_hash,
                expires_at,
            })
            .await?;
        Ok(DevicePairingCreated {
            pairing_id,
            secret,
            expires_at,
        })
    }

    pub async fn cancel(&self, user_id: &str, pairing_id: &str) -> Result<bool, StorageError> {
        self.database
            .cancel_device_pairing(user_id, pairing_id)
            .await
    }

    pub async fn redeem(
        &self,
        pairing_id: &str,
        secret: &str,
        device: &PrismDeviceInfo<'_>,
    ) -> Result<DevicePairingRedemption, DevicePairingError> {
        let access_token = generate_random_value()?;
        let token_hash = hash_value(&access_token);
        let token_id = Uuid::now_v7().to_string();
        let secret_hash = hash_value(secret);
        let result = self
            .database
            .redeem_device_pairing(
                pairing_id,
                &secret_hash,
                NewDeviceAccessToken {
                    id: &token_id,
                    token_hash: &token_hash,
                    device_id: device.device_id,
                    client_name: DEVICE_PAIRING_CLIENT_NAME,
                    device_name: device.device_name,
                    client_version: device.version,
                    device_type: Some(device.platform),
                },
                current_unix_timestamp(),
            )
            .await?;
        match result {
            DevicePairingRedeemResult::Redeemed { user_id } => Ok(DevicePairingRedemption {
                access_token,
                user_id,
            }),
            DevicePairingRedeemResult::NotFound => Err(DevicePairingError::NotFound),
            DevicePairingRedeemResult::InvalidSecret => Err(DevicePairingError::InvalidSecret),
            DevicePairingRedeemResult::Expired => Err(DevicePairingError::Expired),
            DevicePairingRedeemResult::Cancelled => Err(DevicePairingError::Cancelled),
            DevicePairingRedeemResult::Consumed => Err(DevicePairingError::Consumed),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevicePairingCreated {
    pub pairing_id: String,
    pub secret: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevicePairingRedemption {
    pub access_token: String,
    pub user_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrismDeviceInfo<'a> {
    pub device_id: &'a str,
    pub device_name: &'a str,
    pub platform: &'a str,
    pub version: &'a str,
}

#[derive(Debug)]
pub enum DevicePairingError {
    Cancelled,
    Consumed,
    Expired,
    InvalidSecret,
    NotFound,
    Storage(StorageError),
    TokenGeneration(String),
}

impl From<StorageError> for DevicePairingError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl std::fmt::Display for DevicePairingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("device pairing was cancelled"),
            Self::Consumed => formatter.write_str("device pairing was already consumed"),
            Self::Expired => formatter.write_str("device pairing expired"),
            Self::InvalidSecret => formatter.write_str("device pairing secret is invalid"),
            Self::NotFound => formatter.write_str("device pairing was not found"),
            Self::Storage(error) => error.fmt(formatter),
            Self::TokenGeneration(error) => write!(formatter, "token generation failed: {error}"),
        }
    }
}

impl std::error::Error for DevicePairingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Cancelled
            | Self::Consumed
            | Self::Expired
            | Self::InvalidSecret
            | Self::NotFound
            | Self::TokenGeneration(_) => None,
        }
    }
}

fn generate_random_value() -> Result<String, DevicePairingError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| DevicePairingError::TokenGeneration(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn hash_value(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

fn current_unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}
