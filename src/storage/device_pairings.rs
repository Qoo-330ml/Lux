use super::*;

use subtle::ConstantTimeEq;

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum DevicePairingRedeemResult {
    Redeemed { user_id: String },
    NotFound,
    InvalidSecret,
    Expired,
    Cancelled,
    Consumed,
}

impl Database {
    pub(crate) async fn create_device_pairing(
        &self,
        pairing: NewDevicePairing<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO device_pairings (id, user_id, secret_hash, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(pairing.id)
        .bind(pairing.user_id)
        .bind(pairing.secret_hash)
        .bind(pairing.expires_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn cancel_device_pairing(
        &self,
        user_id: &str,
        pairing_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE device_pairings
             SET cancelled_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND user_id = ?
               AND consumed_at IS NULL AND cancelled_at IS NULL",
        )
        .bind(pairing_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn redeem_device_pairing(
        &self,
        pairing_id: &str,
        secret_hash: &[u8],
        token: NewDeviceAccessToken<'_>,
        now: i64,
    ) -> Result<DevicePairingRedeemResult, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let Some(row) = self
            .query(
                "SELECT user_id, secret_hash, expires_at, consumed_at, cancelled_at
                 FROM device_pairings
                 WHERE id = ?",
            )
            .bind(pairing_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        else {
            return Ok(DevicePairingRedeemResult::NotFound);
        };

        let stored_secret_hash: Vec<u8> = row.get("secret_hash");
        if !bool::from(stored_secret_hash.as_slice().ct_eq(secret_hash)) {
            return Ok(DevicePairingRedeemResult::InvalidSecret);
        }
        if row.get::<Option<i64>, _>("cancelled_at").is_some() {
            return Ok(DevicePairingRedeemResult::Cancelled);
        }
        if row.get::<Option<i64>, _>("consumed_at").is_some() {
            return Ok(DevicePairingRedeemResult::Consumed);
        }
        if now >= row.get::<i64, _>("expires_at") {
            return Ok(DevicePairingRedeemResult::Expired);
        }

        let consumed = self
            .query(
                "UPDATE device_pairings
                 SET consumed_at = ?, updated_at = ?
                 WHERE id = ? AND secret_hash = ?
                   AND consumed_at IS NULL AND cancelled_at IS NULL
                   AND expires_at > ?",
            )
            .bind(now)
            .bind(now)
            .bind(pairing_id)
            .bind(secret_hash)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if consumed.rows_affected() != 1 {
            return Ok(DevicePairingRedeemResult::Consumed);
        }

        let user_id: String = row.get("user_id");
        self.query(
            "INSERT INTO access_tokens (
                id, token_hash, user_id, device_id, client_name,
                device_name, client_version, device_type
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(token.id)
        .bind(token.token_hash)
        .bind(&user_id)
        .bind(token.device_id)
        .bind(token.client_name)
        .bind(token.device_name)
        .bind(token.client_version)
        .bind(token.device_type)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(DevicePairingRedeemResult::Redeemed { user_id })
    }
}
