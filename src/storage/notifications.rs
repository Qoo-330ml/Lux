use super::*;

impl Database {
    pub(crate) async fn create_notification_destination(
        &self,
        destination: NewNotificationDestination<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO notification_destinations (
                id, name, url, enabled, allow_private_network, event_types_json, payload_format,
                provider_plugin_id, provider_config_json
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(destination.id)
        .bind(destination.name)
        .bind(destination.url)
        .bind(database_flag(destination.enabled))
        .bind(database_flag(destination.allow_private_network))
        .bind(destination.event_types_json)
        .bind(destination.payload_format)
        .bind(destination.provider_plugin_id)
        .bind(destination.provider_config_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_notification_destination(
        &self,
        id: &str,
    ) -> Result<Option<StoredNotificationDestination>, StorageError> {
        self.query(
            "SELECT id, name, url, enabled, allow_private_network, event_types_json, payload_format,
                    provider_plugin_id, provider_config_json, created_at, updated_at
             FROM notification_destinations WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_notification_destination))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_notification_destinations(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredNotificationDestination>, StorageError> {
        self.query(
            "SELECT id, name, url, enabled, allow_private_network, event_types_json, payload_format,
                    provider_plugin_id, provider_config_json, created_at, updated_at
             FROM notification_destinations
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_notification_destination)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_enabled_notification_destinations(
        &self,
    ) -> Result<Vec<StoredNotificationDestination>, StorageError> {
        self.query(
            "SELECT id, name, url, enabled, allow_private_network, event_types_json, payload_format,
                    provider_plugin_id, provider_config_json, created_at, updated_at
             FROM notification_destinations
             WHERE enabled = 1
             ORDER BY id LIMIT 1000",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_notification_destination)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_notification_destination(
        &self,
        id: &str,
        update: UpdateNotificationDestination<'_>,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE notification_destinations
             SET name = COALESCE(?, name),
                 url = COALESCE(?, url),
                 enabled = COALESCE(?, enabled),
                 allow_private_network = COALESCE(?, allow_private_network),
                 event_types_json = COALESCE(?, event_types_json),
                 payload_format = COALESCE(?, payload_format),
                 provider_plugin_id = COALESCE(?, provider_plugin_id),
                 provider_config_json = COALESCE(?, provider_config_json),
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(update.name)
        .bind(update.url)
        .bind(update.enabled.map(database_flag))
        .bind(update.allow_private_network.map(database_flag))
        .bind(update.event_types_json)
        .bind(update.payload_format)
        .bind(update.provider_plugin_id)
        .bind(update.provider_config_json)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_notification_destination(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query("DELETE FROM notification_destinations WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() == 1)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn insert_notification_event_with_deliveries(
        &self,
        event: NewNotificationEvent<'_>,
        destination_ids: &[String],
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let inserted = self
            .query(
                "INSERT INTO notification_events (
                    id, event_type, schema_version, occurred_at, dedupe_key, payload_json
                 ) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(dedupe_key) DO NOTHING",
            )
            .bind(event.id)
            .bind(event.event_type)
            .bind(event.schema_version)
            .bind(event.occurred_at)
            .bind(event.dedupe_key)
            .bind(event.payload_json)
            .execute(&mut *transaction)
            .await
            .map(|result| result.rows_affected() == 1)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if inserted {
            for destination_id in destination_ids {
                self.query(
                    "INSERT INTO notification_deliveries (
                        id, event_id, destination_id, status
                     ) VALUES (?, ?, ?, 'PENDING')
                     ON CONFLICT(event_id, destination_id) DO NOTHING",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(event.id)
                .bind(destination_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(inserted)
    }

    pub(crate) async fn list_ready_notification_deliveries(
        &self,
        limit: i64,
    ) -> Result<Vec<StoredNotificationDelivery>, StorageError> {
        self.query(
            "SELECT d.id, d.event_id, d.destination_id, d.status, d.attempt_count,
                    d.next_attempt_at, d.claimed_until, d.last_http_status, d.last_error,
                    d.delivered_at, d.created_at, d.updated_at,
                    e.event_type, e.schema_version, e.occurred_at, e.payload_json,
                    n.name, n.url, n.allow_private_network,
                    n.provider_plugin_id, n.provider_config_json
             FROM notification_deliveries d
             JOIN notification_events e ON e.id = d.event_id
             JOIN notification_destinations n ON n.id = d.destination_id
             WHERE n.enabled = 1
               AND d.next_attempt_at <= unixepoch()
               AND (d.status = 'PENDING'
                    OR (d.status = 'RUNNING' AND d.claimed_until <= unixepoch()))
             ORDER BY d.next_attempt_at, d.created_at, d.id
             LIMIT ?",
        )
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_notification_delivery).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_notification_deliveries(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredNotificationDelivery>, StorageError> {
        self.query(
            "SELECT d.id, d.event_id, d.destination_id, d.status, d.attempt_count,
                    d.next_attempt_at, d.claimed_until, d.last_http_status, d.last_error,
                    d.delivered_at, d.created_at, d.updated_at,
                    e.event_type, e.schema_version, e.occurred_at, e.payload_json,
                    n.name, n.url, n.allow_private_network,
                    n.provider_plugin_id, n.provider_config_json
             FROM notification_deliveries d
             JOIN notification_events e ON e.id = d.event_id
             JOIN notification_destinations n ON n.id = d.destination_id
             ORDER BY d.created_at DESC, d.id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_notification_delivery).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_notification_delivery(
        &self,
        id: &str,
        lease_seconds: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE notification_deliveries
             SET status = 'RUNNING',
                 attempt_count = attempt_count + 1,
                 claimed_until = unixepoch() + ?,
                 updated_at = unixepoch()
             WHERE id = ? AND (
                 status = 'PENDING'
                 OR (status = 'RUNNING' AND claimed_until <= unixepoch())
             )",
        )
        .bind(lease_seconds.max(1))
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_notification_delivered(
        &self,
        id: &str,
        http_status: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE notification_deliveries
             SET status = 'DELIVERED', claimed_until = NULL, last_http_status = ?,
                 last_error = NULL, delivered_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(http_status)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_notification_retry(
        &self,
        id: &str,
        status: &str,
        http_status: Option<i64>,
        error: &str,
        next_attempt_at: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE notification_deliveries
             SET status = ?, claimed_until = NULL, last_http_status = ?, last_error = ?,
                 next_attempt_at = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(status)
        .bind(http_status)
        .bind(error)
        .bind(next_attempt_at)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn retry_notification_delivery(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE notification_deliveries
             SET status = 'PENDING', next_attempt_at = unixepoch(), claimed_until = NULL,
                 last_error = NULL, updated_at = unixepoch()
             WHERE id = ? AND status IN ('FAILED', 'DELIVERED')",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}
