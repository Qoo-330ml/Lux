use super::*;

impl Database {
    pub(crate) async fn list_current_metadata_reidentify_items(
        &self,
        job_ids: &[String],
    ) -> Result<Vec<(String, StoredJobActivityItem)>, StorageError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", job_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT job_items.job_id, items.item_type, items.season_number,
                        items.episode_number, items.title, series.title AS series_title,
                        ROW_NUMBER() OVER (
                            PARTITION BY job_items.job_id
                            ORDER BY CASE WHEN job_items.status = 'RUNNING' THEN 0 ELSE 1 END,
                                     job_items.updated_at DESC, job_items.item_id
                        ) AS activity_rank
                 FROM metadata_reidentify_job_items job_items
                 JOIN media_items items ON items.id = job_items.item_id
                 LEFT JOIN media_items series ON series.id = items.series_id
                 WHERE job_items.job_id IN ({placeholders})
                   AND job_items.status IN ('PENDING', 'RUNNING')
             )
             SELECT job_id, item_type, season_number, episode_number, title, series_title
             FROM ranked WHERE activity_rank = 1"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for job_id in job_ids {
            statement = statement.bind(job_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get("job_id"),
                            StoredJobActivityItem {
                                item_type: row.get("item_type"),
                                season_number: row.get("season_number"),
                                episode_number: row.get("episode_number"),
                                title: row.get("title"),
                                series_title: row.get("series_title"),
                            },
                        )
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_current_chapter_detection_items(
        &self,
        job_ids: &[String],
    ) -> Result<Vec<(String, StoredJobActivityItem)>, StorageError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", job_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT job_items.job_id, items.item_type, items.season_number,
                        items.episode_number, items.title, series.title AS series_title,
                        ROW_NUMBER() OVER (
                            PARTITION BY job_items.job_id
                            ORDER BY CASE WHEN job_items.status = 'RUNNING' THEN 0 ELSE 1 END,
                                     job_items.updated_at DESC, job_items.source_id
                        ) AS activity_rank
                 FROM chapter_detection_job_items job_items
                 JOIN media_items items ON items.id = job_items.item_id
                 LEFT JOIN media_items series ON series.id = items.series_id
                 WHERE job_items.job_id IN ({placeholders})
                   AND job_items.status IN ('PENDING', 'RUNNING')
             )
             SELECT job_id, item_type, season_number, episode_number, title, series_title
             FROM ranked WHERE activity_rank = 1"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for job_id in job_ids {
            statement = statement.bind(job_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get("job_id"),
                            StoredJobActivityItem {
                                item_type: row.get("item_type"),
                                season_number: row.get("season_number"),
                                episode_number: row.get("episode_number"),
                                title: row.get("title"),
                                series_title: row.get("series_title"),
                            },
                        )
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_current_danmaku_match_items(
        &self,
        job_ids: &[String],
    ) -> Result<Vec<(String, StoredJobActivityItem)>, StorageError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", job_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT job_items.job_id, items.item_type, items.season_number,
                        items.episode_number, items.title, series.title AS series_title,
                        ROW_NUMBER() OVER (
                            PARTITION BY job_items.job_id
                            ORDER BY CASE WHEN job_items.status = 'RUNNING' THEN 0 ELSE 1 END,
                                     job_items.updated_at DESC, job_items.id
                        ) AS activity_rank
                 FROM danmaku_match_job_items job_items
                 JOIN media_sources sources ON sources.id = job_items.media_source_id
                 JOIN media_items items ON items.id = sources.item_id
                 LEFT JOIN media_items series ON series.id = items.series_id
                 WHERE job_items.job_id IN ({placeholders})
                   AND job_items.status IN ('PENDING', 'RUNNING')
             )
             SELECT job_id, item_type, season_number, episode_number, title, series_title
             FROM ranked WHERE activity_rank = 1"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for job_id in job_ids {
            statement = statement.bind(job_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get("job_id"),
                            StoredJobActivityItem {
                                item_type: row.get("item_type"),
                                season_number: row.get("season_number"),
                                episode_number: row.get("episode_number"),
                                title: row.get("title"),
                                series_title: row.get("series_title"),
                            },
                        )
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    #[cfg(test)]
    pub(crate) async fn next_metadata_reidentify_item(
        &self,
        job_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "WITH active_priority AS (
                 SELECT MIN(priority) AS priority
                 FROM (
                     SELECT MIN(priority) AS priority
                     FROM metadata_reidentify_job_items
                     WHERE job_id = ? AND status = 'PENDING'
                     UNION ALL
                     SELECT MIN(priority) AS priority
                     FROM metadata_reidentify_job_items
                     WHERE job_id = ? AND status = 'RUNNING'
                 ) priorities
             )
             SELECT item_id
             FROM metadata_reidentify_job_items
             WHERE job_id = ? AND status = 'PENDING'
               AND priority = (SELECT priority FROM active_priority)
             ORDER BY item_id
             LIMIT 1",
        )
        .bind(job_id)
        .bind(job_id)
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Claims up to `limit` metadata items in one write transaction.
    ///
    /// Keeping the priority selection and status updates in the same
    /// transaction avoids one read, one write transaction, and one commit per
    /// worker slot while preserving the existing series/season/episode order.
    pub(crate) async fn claim_next_metadata_reidentify_items(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<String>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let mut claimed = self
            .query_scalar::<String>(
                "WITH active_priority AS (
                     SELECT MIN(priority) AS priority
                     FROM (
                         SELECT MIN(priority) AS priority
                         FROM metadata_reidentify_job_items
                         WHERE job_id = ? AND status = 'PENDING'
                         UNION ALL
                         SELECT MIN(priority) AS priority
                         FROM metadata_reidentify_job_items
                         WHERE job_id = ? AND status = 'RUNNING'
                     ) priorities
                 ), eligible AS (
                     SELECT item_id
                     FROM metadata_reidentify_job_items
                     WHERE job_id = ? AND status = 'PENDING'
                       AND priority = (SELECT priority FROM active_priority)
                     ORDER BY item_id
                     LIMIT ?
                 )
                 UPDATE metadata_reidentify_job_items
                 SET status = 'RUNNING',
                     claimed_request_fingerprint = request_fingerprint,
                     claimed_request_capabilities_json = request_capabilities_json,
                     updated_at = unixepoch()
                 WHERE job_id = ? AND status = 'PENDING'
                   AND item_id IN (SELECT item_id FROM eligible)
                   AND EXISTS (
                       SELECT 1 FROM metadata_reidentify_jobs
                       WHERE id = ? AND status IN ('QUEUED', 'RUNNING')
                         AND cancel_requested = 0
                   )
                 RETURNING item_id",
            )
            .bind(job_id)
            .bind(job_id)
            .bind(job_id)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .bind(job_id)
            .bind(job_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        claimed.sort_unstable();
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(claimed)
    }

    pub(crate) async fn finish_metadata_reidentify_item(
        &self,
        job_id: &str,
        item_id: &str,
        status: &str,
        candidate_count: i64,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let query = "WITH changed_metadata_fill_request AS (
                         SELECT item.job_id, item.item_id
                         FROM metadata_reidentify_job_items item
                         JOIN metadata_reidentify_jobs job ON job.id = item.job_id
                         WHERE item.job_id = ? AND item.item_id = ?
                           AND item.status = 'RUNNING'
                           AND job.mode = 'FILL_MISSING' AND job.cancel_requested = 0
                           AND (
                               item.request_fingerprint <> item.claimed_request_fingerprint
                               OR (item.request_fingerprint IS NULL
                                   AND item.claimed_request_fingerprint IS NOT NULL)
                               OR (item.request_fingerprint IS NOT NULL
                                   AND item.claimed_request_fingerprint IS NULL)
                               OR item.request_capabilities_json <>
                                  item.claimed_request_capabilities_json
                           )
                     ), provider_unavailable_metadata_fill AS (
                         SELECT item.job_id, item.item_id
                         FROM metadata_reidentify_job_items item
                         JOIN metadata_reidentify_jobs job ON job.id = item.job_id
                         WHERE item.job_id = ? AND item.item_id = ?
                           AND item.status = 'RUNNING'
                           AND job.mode = 'FILL_MISSING'
                           AND job.cancel_requested = 0
                           AND item.request_fingerprint IS NOT NULL
                           AND ? = 'FAILED'
                           AND ? = 'SCRAPER_UNAVAILABLE'
                           AND NOT EXISTS (
                               SELECT 1 FROM changed_metadata_fill_request
                           )
                     )
                     UPDATE metadata_reidentify_job_items
                     SET status = CASE WHEN EXISTS (
                             SELECT 1 FROM changed_metadata_fill_request
                         ) THEN 'PENDING' ELSE ? END,
                         candidate_count = CASE WHEN EXISTS (
                             SELECT 1 FROM changed_metadata_fill_request
                         ) THEN 0 ELSE ? END,
                         error = CASE WHEN EXISTS (
                             SELECT 1 FROM changed_metadata_fill_request
                         ) THEN NULL ELSE ? END,
                         automatic_retry_consumed = CASE
                             WHEN EXISTS (SELECT 1 FROM provider_unavailable_metadata_fill)
                               OR EXISTS (SELECT 1 FROM changed_metadata_fill_request)
                                 THEN 0
                             ELSE automatic_retry_consumed
                         END,
                         automatic_retry_count = CASE
                             WHEN EXISTS (SELECT 1 FROM provider_unavailable_metadata_fill)
                                 THEN CASE WHEN automatic_retry_count < 3
                                     THEN automatic_retry_count + 1
                                     ELSE automatic_retry_count END
                             WHEN EXISTS (SELECT 1 FROM changed_metadata_fill_request)
                                 THEN 0
                             ELSE automatic_retry_count
                         END,
                         automatic_retry_after = CASE
                             WHEN EXISTS (SELECT 1 FROM provider_unavailable_metadata_fill)
                                 THEN unixepoch() + CASE automatic_retry_count
                                     WHEN 0 THEN 300
                                     WHEN 1 THEN 1800
                                     ELSE 21600
                                 END
                             WHEN EXISTS (SELECT 1 FROM changed_metadata_fill_request)
                                 THEN NULL
                             ELSE automatic_retry_after
                         END,
                         claimed_request_fingerprint = NULL,
                         claimed_request_capabilities_json = '[]',
                         updated_at = unixepoch()
                     WHERE job_id = ? AND item_id = ? AND status = 'RUNNING'
                     RETURNING status";
        let item_status = self
            .query_scalar::<String>(query)
            .bind(job_id)
            .bind(item_id)
            .bind(job_id)
            .bind(item_id)
            .bind(status)
            .bind(error)
            .bind(status)
            .bind(candidate_count)
            .bind(error)
            .bind(job_id)
            .bind(item_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if item_status
            .as_deref()
            .is_some_and(|item_status| item_status != "PENDING")
        {
            self.query(
                "UPDATE metadata_reidentify_jobs
                 SET processed_count = processed_count + 1, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(job_id)
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
            })
    }

    pub(crate) async fn fail_running_metadata_reidentify_items(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<i64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_job_items
                 SET status = 'FAILED', candidate_count = 0, error = ?,
                     claimed_request_fingerprint = NULL,
                     claimed_request_capabilities_json = '[]',
                     updated_at = unixepoch()
                 WHERE job_id = ? AND status = 'RUNNING'",
            )
            .bind(error)
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let affected = i64::try_from(result.rows_affected()).unwrap_or(i64::MAX);
        if affected > 0 {
            self.query(
                "UPDATE metadata_reidentify_jobs
                 SET processed_count = processed_count + ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(affected)
            .bind(job_id)
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
        Ok(affected)
    }

    pub(crate) async fn requeue_running_metadata_reidentify_items(
        &self,
        job_id: &str,
    ) -> Result<u64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_job_items
             SET status = 'PENDING', error = NULL,
                 claimed_request_fingerprint = NULL,
                 claimed_request_capabilities_json = '[]',
                 updated_at = unixepoch()
             WHERE job_id = ? AND status = 'RUNNING'",
            )
            .bind(job_id)
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
        Ok(result.rows_affected())
    }
}
