use super::*;

impl Database {
    pub(crate) async fn list_scan_fingerprint_migration_library_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar("SELECT id FROM libraries WHERE system_kind IS NULL ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn ensure_scan_fingerprint_migration_state(
        &self,
        library_id: &str,
    ) -> Result<StoredScanFingerprintMigrationCursor, StorageError> {
        self.query(
            "INSERT INTO scan_fingerprint_migration_state (library_id)
             VALUES (?) ON CONFLICT (library_id) DO NOTHING",
        )
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let row = self
            .query(
                "SELECT cursor_root_id, cursor_relative_path, completed_at
                 FROM scan_fingerprint_migration_state WHERE library_id = ?",
            )
            .bind(library_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(row.map_or(
            StoredScanFingerprintMigrationCursor {
                root_id: None,
                relative_path: None,
                completed_at: None,
            },
            |row| StoredScanFingerprintMigrationCursor {
                root_id: row.get("cursor_root_id"),
                relative_path: row.get("cursor_relative_path"),
                completed_at: row.get("completed_at"),
            },
        ))
    }

    pub(crate) async fn find_scan_fingerprint_migration_candidate(
        &self,
        library_id: &str,
        cursor: &StoredScanFingerprintMigrationCursor,
    ) -> Result<Option<StoredScanFingerprintMigrationCandidate>, StorageError> {
        let base = "SELECT fe.id AS entry_id, lr.library_id, fe.library_root_id,
                           fe.relative_path, lr.canonical_path AS root_path,
                           lr.is_available AS root_is_available, fe.fingerprint,
                           COALESCE(deferred.attempts, 0) AS attempts
                    FROM filesystem_entries fe
                    JOIN library_roots lr ON lr.id = fe.library_root_id
                    LEFT JOIN scan_fingerprint_migration_deferred deferred
                      ON deferred.filesystem_entry_id = fe.id
                    WHERE lr.library_id = ? AND fe.entry_kind = 'FILE'
                      AND lr.is_available = 1 AND fe.is_missing = 0
                      AND length(fe.fingerprint) = 32
                      AND NOT EXISTS (
                          SELECT 1 FROM scan_fingerprint_migration_deferred_roots root_deferred
                          WHERE root_deferred.library_root_id = lr.id
                            AND root_deferred.retry_at > unixepoch()
                      )
                      AND (deferred.retry_at IS NULL OR deferred.retry_at <= unixepoch())";
        let (query, cursor_binds) = match (&cursor.root_id, &cursor.relative_path) {
            (Some(root_id), Some(relative_path)) => (
                format!(
                    "{base} AND (fe.library_root_id > ? OR
                         (fe.library_root_id = ? AND fe.relative_path > ?))
                     ORDER BY fe.library_root_id, fe.relative_path LIMIT 1"
                ),
                Some((root_id, relative_path)),
            ),
            _ => (
                format!("{base} ORDER BY fe.library_root_id, fe.relative_path LIMIT 1"),
                None,
            ),
        };
        let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_id);
        if let Some((root_id, relative_path)) = cursor_binds {
            statement = statement.bind(root_id).bind(root_id).bind(relative_path);
        }
        statement
            .fetch_optional(&self.pool)
            .await
            .map(|row| {
                row.map(|row| StoredScanFingerprintMigrationCandidate {
                    entry_id: row.get("entry_id"),
                    library_id: row.get("library_id"),
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                    root_path: row.get("root_path"),
                    root_is_available: row.get::<i64, _>("root_is_available") != 0,
                    fingerprint: row.get("fingerprint"),
                    attempts: row.get("attempts"),
                })
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn scan_fingerprint_migration_has_pending_entries(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM filesystem_entries fe
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE lr.library_id = ? AND fe.entry_kind = 'FILE'
                   AND ((fe.is_missing = 0 AND length(fe.fingerprint) = 32)
                        OR (fe.is_missing = 1 AND fe.fingerprint IS NOT NULL))
             ) THEN 1 ELSE 0 END",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn clear_one_missing_scan_fingerprint(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE filesystem_entries
             SET fingerprint = NULL, updated_at = unixepoch()
             WHERE id = (
                 SELECT fe.id FROM filesystem_entries fe
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE lr.library_id = ? AND fe.entry_kind = 'FILE'
                   AND fe.is_missing = 1 AND fe.fingerprint IS NOT NULL
                 ORDER BY fe.library_root_id, fe.relative_path LIMIT 1
             )
               AND entry_kind = 'FILE' AND is_missing = 1 AND fingerprint IS NOT NULL",
        )
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn advance_scan_fingerprint_migration_cursor(
        &self,
        library_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_fingerprint_migration_state
             SET cursor_root_id = ?, cursor_relative_path = ?, completed_at = NULL,
                 updated_at = unixepoch()
             WHERE library_id = ?",
        )
        .bind(root_id)
        .bind(relative_path)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_scan_fingerprint_migration_lease(
        &self,
        library_id: &str,
        lease_token: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO scan_fingerprint_migration_state (library_id)
             VALUES (?) ON CONFLICT (library_id) DO NOTHING",
        )
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE scan_fingerprint_migration_state SET updated_at = updated_at
             WHERE library_id = ?",
        )
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let lease_is_active: i64 = self
            .query_scalar(
                "SELECT CASE WHEN lease_token IS NOT NULL
                                  AND lease_expires_at > unixepoch()
                             THEN 1 ELSE 0 END
                 FROM scan_fingerprint_migration_state WHERE library_id = ?",
            )
            .bind(library_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if lease_is_active != 0 {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        let active_scan_job: i64 = self
            .query_scalar(
                "SELECT CASE WHEN EXISTS (
                     SELECT 1 FROM scan_jobs job
                     WHERE job.library_id = ?
                       AND (job.status IN ('PENDING', 'RUNNING')
                            OR (job.status = 'COMPLETED' AND job.scan_phase = 'POSTPROCESSING'))
                 ) THEN 1 ELSE 0 END",
            )
            .bind(library_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let active_chapter_job: i64 = self
            .query_scalar(
                "SELECT CASE WHEN EXISTS (
                     SELECT 1 FROM chapter_detection_jobs job
                     WHERE job.library_id = ? AND job.status IN ('PENDING', 'RUNNING')
                 ) THEN 1 ELSE 0 END",
            )
            .bind(library_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if active_scan_job != 0 || active_chapter_job != 0 {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        let result = self
            .query(
                "UPDATE scan_fingerprint_migration_state
                 SET lease_token = ?, lease_expires_at = unixepoch() + 30,
                     updated_at = unixepoch()
                 WHERE library_id = ?",
            )
            .bind(lease_token)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let claimed = result.rows_affected() == 1;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(claimed)
    }

    pub(crate) async fn release_scan_fingerprint_migration_lease(
        &self,
        library_id: &str,
        lease_token: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_fingerprint_migration_state
             SET lease_token = NULL, lease_expires_at = NULL, updated_at = unixepoch()
             WHERE library_id = ? AND lease_token = ?",
        )
        .bind(library_id)
        .bind(lease_token)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn defer_scan_fingerprint_migration_root(
        &self,
        root_id: &str,
        retry_at: i64,
        reason: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO scan_fingerprint_migration_deferred_roots (
                library_root_id, retry_at, reason
             ) VALUES (?, ?, ?)
             ON CONFLICT (library_root_id) DO UPDATE SET
                retry_at = excluded.retry_at, reason = excluded.reason,
                updated_at = unixepoch()",
        )
        .bind(root_id)
        .bind(retry_at)
        .bind(reason)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn reset_scan_fingerprint_migration_cursor(
        &self,
        library_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_fingerprint_migration_state
             SET cursor_root_id = NULL, cursor_relative_path = NULL,
                 completed_at = NULL, updated_at = unixepoch()
             WHERE library_id = ?",
        )
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn complete_scan_fingerprint_migration_library(
        &self,
        library_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_fingerprint_migration_state
             SET completed_at = unixepoch(), updated_at = unixepoch()
             WHERE library_id = ? AND NOT EXISTS (
                 SELECT 1 FROM filesystem_entries fe
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE lr.library_id = ? AND fe.entry_kind = 'FILE'
                   AND fe.is_missing = 0 AND length(fe.fingerprint) = 32
             ) AND NOT EXISTS (
                 SELECT 1 FROM filesystem_entries fe
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE lr.library_id = ? AND fe.entry_kind = 'FILE'
                   AND fe.is_missing = 1 AND fe.fingerprint IS NOT NULL
             )",
        )
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn defer_scan_fingerprint_migration_candidate(
        &self,
        candidate: &StoredScanFingerprintMigrationCandidate,
        retry_at: i64,
        reason: &str,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO scan_fingerprint_migration_deferred (
                filesystem_entry_id, attempts, retry_at, reason
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT (filesystem_entry_id) DO UPDATE SET
                attempts = scan_fingerprint_migration_deferred.attempts + 1,
                retry_at = excluded.retry_at, reason = excluded.reason,
                updated_at = unixepoch()",
        )
        .bind(&candidate.entry_id)
        .bind(candidate.attempts.saturating_add(1))
        .bind(retry_at)
        .bind(reason)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE scan_fingerprint_migration_state
             SET cursor_root_id = ?, cursor_relative_path = ?, completed_at = NULL,
                 updated_at = unixepoch()
             WHERE library_id = ?",
        )
        .bind(&candidate.library_root_id)
        .bind(&candidate.relative_path)
        .bind(&candidate.library_id)
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
            })
    }

    pub(crate) async fn migrate_scan_fingerprint_candidate(
        &self,
        candidate: &StoredScanFingerprintMigrationCandidate,
        fingerprint_v2: &[u8],
        size: i64,
        modified_at: i64,
        device: i64,
        inode: i64,
        lease_token: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "UPDATE scan_fingerprint_migration_state SET updated_at = updated_at
             WHERE library_id = ?",
        )
        .bind(&candidate.library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let owns_active_lease: i64 = self
            .query_scalar(
                "SELECT CASE WHEN lease_token = ? AND lease_expires_at > unixepoch()
                             THEN 1 ELSE 0 END
                 FROM scan_fingerprint_migration_state WHERE library_id = ?",
            )
            .bind(lease_token)
            .bind(&candidate.library_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if owns_active_lease == 0 {
            self.query(
                "UPDATE scan_fingerprint_migration_state
                 SET lease_token = NULL, lease_expires_at = NULL, updated_at = unixepoch()
                 WHERE library_id = ? AND lease_token = ?",
            )
            .bind(&candidate.library_id)
            .bind(lease_token)
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
            return Ok(false);
        }
        let result = self
            .query(
                "UPDATE filesystem_entries
                 SET fingerprint = ?, size = ?, modified_at = ?, device = ?, inode = ?,
                     updated_at = unixepoch()
                 WHERE id = ? AND library_root_id = ? AND relative_path = ?
                   AND entry_kind = 'FILE' AND is_missing = 0 AND fingerprint = ?
                   AND EXISTS (
                       SELECT 1 FROM scan_fingerprint_migration_state state
                       WHERE state.library_id = ? AND state.lease_token = ?
                         AND state.lease_expires_at > unixepoch()
                   )",
            )
            .bind(fingerprint_v2)
            .bind(size)
            .bind(modified_at)
            .bind(device)
            .bind(inode)
            .bind(&candidate.entry_id)
            .bind(&candidate.library_root_id)
            .bind(&candidate.relative_path)
            .bind(&candidate.fingerprint)
            .bind(&candidate.library_id)
            .bind(lease_token)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let migrated = result.rows_affected() == 1;
        if migrated {
            self.query(
                "DELETE FROM scan_fingerprint_migration_deferred
                 WHERE filesystem_entry_id = ?",
            )
            .bind(&candidate.entry_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "UPDATE scan_fingerprint_migration_state
                 SET cursor_root_id = ?, cursor_relative_path = ?, completed_at = NULL,
                     lease_token = NULL, lease_expires_at = NULL,
                     updated_at = unixepoch()
                 WHERE library_id = ? AND lease_token = ?",
            )
            .bind(&candidate.library_root_id)
            .bind(&candidate.relative_path)
            .bind(&candidate.library_id)
            .bind(lease_token)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        } else {
            self.query(
                "UPDATE scan_fingerprint_migration_state
                 SET lease_token = NULL, lease_expires_at = NULL, updated_at = unixepoch()
                 WHERE library_id = ? AND lease_token = ?",
            )
            .bind(&candidate.library_id)
            .bind(lease_token)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(migrated)
    }
}
