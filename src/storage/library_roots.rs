use super::*;

impl Database {
    pub(crate) async fn insert_library_root(
        &self,
        root: NewLibraryRoot<'_>,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        let active_library = self
            .query(
                "UPDATE libraries SET updated_at = updated_at
                 WHERE id = ? AND deletion_state = 'ACTIVE'",
            )
            .bind(root.library_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if active_library.rows_affected() == 0 {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(false);
        }
        self.query(
            "INSERT INTO library_roots (
                id, library_id, canonical_path, display_path, is_available, is_writable
            ) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(root.id)
        .bind(root.library_id)
        .bind(root.canonical_path)
        .bind(root.display_path)
        .bind(database_flag(root.is_available))
        .bind(database_flag(root.is_writable))
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(true)
    }

    pub(crate) async fn list_library_roots(
        &self,
        library_id: &str,
    ) -> Result<Vec<StoredLibraryRoot>, StorageError> {
        self.query(
            "SELECT id, library_id, canonical_path, display_path,
                    is_available, is_writable, last_checked_at,
                    unavailable_since, scan_cursor
             FROM library_roots WHERE library_id = ?
             ORDER BY canonical_path, id",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_library_root).collect())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_library_roots_by_ids(
        &self,
        root_ids: &[String],
    ) -> Result<HashMap<String, StoredLibraryRoot>, StorageError> {
        if root_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut roots = HashMap::with_capacity(root_ids.len());
        for root_ids in root_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", root_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, library_id, canonical_path, display_path,
                        is_available, is_writable, last_checked_at,
                        unavailable_since, scan_cursor
                 FROM library_roots
                 WHERE id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for root_id in root_ids {
                statement = statement.bind(root_id);
            }
            let rows = statement
                .fetch_all(&self.pool)
                .await
                .map_err(self.sqlx_err())?;
            for row in rows {
                let root = stored_library_root(row);
                roots.insert(root.id.clone(), root);
            }
        }
        Ok(roots)
    }

    pub(crate) async fn list_library_roots_by_library_ids(
        &self,
        library_ids: &[String],
    ) -> Result<HashMap<String, Vec<StoredLibraryRoot>>, StorageError> {
        if library_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut roots = HashMap::<String, Vec<StoredLibraryRoot>>::new();
        for library_ids in library_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", library_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, library_id, canonical_path, display_path,
                        is_available, is_writable, last_checked_at,
                        unavailable_since, scan_cursor
                 FROM library_roots
                 WHERE library_id IN ({placeholders})
                 ORDER BY library_id, canonical_path, id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for library_id in library_ids {
                statement = statement.bind(library_id);
            }
            let rows = statement
                .fetch_all(&self.pool)
                .await
                .map_err(self.sqlx_err())?;
            for row in rows {
                let library_id: String = row.get("library_id");
                roots
                    .entry(library_id)
                    .or_default()
                    .push(stored_library_root(row));
            }
        }
        Ok(roots)
    }

    pub(crate) async fn delete_library_root(
        &self,
        library_id: &str,
        root_id: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        let active_library = self
            .query(
                "UPDATE libraries SET updated_at = updated_at
                 WHERE id = ? AND deletion_state = 'ACTIVE'",
            )
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if active_library.rows_affected() == 0 {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(false);
        }
        let history = self
            .query(
                "INSERT INTO library_root_history (library_id, canonical_path, root_id)
                 SELECT library_id, canonical_path, id
                 FROM library_roots
                 WHERE id = ? AND library_id = ?
                 ON CONFLICT(library_id, canonical_path) DO UPDATE SET
                     root_id = excluded.root_id,
                     deleted_at = unixepoch()",
            )
            .bind(root_id)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if history.rows_affected() == 0 {
            transaction.rollback().await.map_err(self.sqlx_err())?;
            return Ok(false);
        }
        self.query("DELETE FROM library_roots WHERE id = ? AND library_id = ?")
            .bind(root_id)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(true)
    }

    pub(crate) async fn find_deleted_library_root_id(
        &self,
        library_id: &str,
        canonical_path: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT root_id
             FROM library_root_history
             WHERE library_id = ? AND canonical_path = ?",
        )
        .bind(library_id)
        .bind(canonical_path)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn delete_library_root_history(
        &self,
        library_id: &str,
        canonical_path: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "DELETE FROM library_root_history
             WHERE library_id = ? AND canonical_path = ?",
        )
        .bind(library_id)
        .bind(canonical_path)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_all_library_roots(
        &self,
    ) -> Result<Vec<StoredLibraryRoot>, StorageError> {
        self.query(
            "SELECT id, library_id, canonical_path, display_path,
                    is_available, is_writable, last_checked_at,
                    unavailable_since, scan_cursor
             FROM library_roots ORDER BY canonical_path, id",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_library_root).collect())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_enabled_library_roots(
        &self,
    ) -> Result<Vec<StoredLibraryRoot>, StorageError> {
        self.query(
            "SELECT lr.id, lr.library_id, lr.canonical_path, lr.display_path,
                    lr.is_available, lr.is_writable, lr.last_checked_at,
                    lr.unavailable_since, lr.scan_cursor
             FROM library_roots lr
             JOIN libraries l ON l.id = lr.library_id
             WHERE l.is_enabled = 1
               AND l.realtime_watch_enabled = 1
             ORDER BY lr.canonical_path, lr.id",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_library_root).collect())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn update_library_last_scan(
        &self,
        library_id: &str,
    ) -> Result<(), StorageError> {
        self.query("UPDATE libraries SET last_scan_at = unixepoch() WHERE id = ?")
            .bind(library_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn update_root_scan_cursor(
        &self,
        root_id: &str,
        cursor: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query("UPDATE library_roots SET scan_cursor = ? WHERE id = ?")
            .bind(cursor)
            .bind(root_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_library_root(
        &self,
        id: &str,
    ) -> Result<Option<StoredLibraryRoot>, StorageError> {
        self.query(
            "SELECT id, library_id, canonical_path, display_path,
                    is_available, is_writable, last_checked_at,
                    unavailable_since, scan_cursor
             FROM library_roots WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_library_root))
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn update_library_root_availability(
        &self,
        root_id: &str,
        is_available: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE library_roots
             SET is_available = ?, last_checked_at = unixepoch(),
                 unavailable_since = CASE
                     WHEN ? = 1 THEN NULL
                     ELSE COALESCE(unavailable_since, unixepoch())
                 END
             WHERE id = ?",
        )
        .bind(database_flag(is_available))
        .bind(database_flag(is_available))
        .bind(root_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }
}
