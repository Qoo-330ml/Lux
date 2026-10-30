use super::*;

/// One statement stays below SQLite's historical 999-variable limit while keeping merges batched.
pub(crate) const MAX_MERGE_RECORD_ROWS_PER_STATEMENT: usize = 100;
/// Merge accepts at most 100 roots, so a candidate group never needs more items than this.
pub(crate) const MAX_IDENTITY_MERGE_GROUP_ITEMS: i64 = 100;
/// Upper bound for one page of identity keys or administrator merge records.
pub(crate) const MAX_IDENTITY_MERGE_KEY_PAGE_SIZE: i64 = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MediaMergeOrigin {
    Manual,
    Automatic,
}

impl MediaMergeOrigin {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Manual => "MANUAL",
            Self::Automatic => "AUTO",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredMediaMergeRecord {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) item_type: String,
    pub(crate) primary_item_id: String,
    pub(crate) merged_item_id: String,
    pub(crate) merged_item_title: String,
    pub(crate) origin: String,
    pub(crate) merged_at: i64,
    pub(crate) undone_at: Option<i64>,
    pub(crate) source_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredMediaMergeUndo {
    pub(crate) library_id: String,
    pub(crate) item_type: String,
    pub(crate) primary_item_id: String,
    pub(crate) merged_item_id: String,
    pub(crate) restored_item_ids: Vec<String>,
    pub(crate) restored_source_count: usize,
}

/// Identity of one automatic-merge candidate group.
///
/// Provider keys carry `provider`/`provider_id`; the title fallback carries
/// `sort_title`/`production_year`. Exactly one pair is populated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredIdentityMergeCandidateKey {
    pub(crate) library_id: String,
    pub(crate) item_type: String,
    pub(crate) provider: Option<String>,
    pub(crate) provider_id: Option<String>,
    pub(crate) sort_title: Option<String>,
    pub(crate) production_year: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredIdentityMergeCandidateItem {
    pub(crate) id: String,
    pub(crate) added_at: i64,
    pub(crate) source_count: i64,
    pub(crate) default_source_count: i64,
    /// Explicit edition markers (`Director's Cut`, ...) protect an item from being auto-merged.
    pub(crate) edition_source_count: i64,
}

struct MergeItemStateRow {
    record_id: String,
    item_id: String,
    item_type: String,
    parent_id: Option<String>,
    series_id: Option<String>,
    merged_into_item_id: Option<String>,
}

/// Row of the item-state snapshot inside `record_media_merges_in_transaction`.
///
/// `owner_id` is the merge root the row belongs to: the item itself for roots, `series_id`
/// for seasons and episodes. It only routes the row onto a record, never leaves this module.
#[derive(Debug, sqlx::FromRow)]
struct MergeItemStateSelectRow {
    id: String,
    item_type: String,
    parent_id: Option<String>,
    series_id: Option<String>,
    merged_into_item_id: Option<String>,
    owner_id: String,
}

/// Row of the media-source snapshot inside `record_media_merges_in_transaction`.
#[derive(Debug, sqlx::FromRow)]
struct MergeSourceStateSelectRow {
    id: String,
    item_id: String,
    is_default: i64,
}

/// Row of the saved hierarchy of one merged item, replayed by `unmerge_media_items`.
#[derive(Debug, sqlx::FromRow)]
struct MergeItemStateRestoreRow {
    item_id: String,
    parent_id: Option<String>,
    series_id: Option<String>,
    merged_into_item_id: Option<String>,
}

/// Row of one saved media source, replayed by `unmerge_media_items`.
#[derive(Debug, sqlx::FromRow)]
struct MergeSourceStateRestoreRow {
    source_id: String,
    item_id: String,
    is_default: i64,
}

/// Row of `list_media_merge_records_for_item`.
///
/// The column is spelled `source` in the table; [`StoredMediaMergeRecord`] exposes it as
/// `origin`, so the conversion lives in one place.
#[derive(Debug, sqlx::FromRow)]
struct MediaMergeRecordRow {
    id: String,
    library_id: String,
    item_type: String,
    primary_item_id: String,
    merged_item_id: String,
    merged_item_title: String,
    source: String,
    merged_at: i64,
    undone_at: Option<i64>,
    source_count: i64,
}

impl From<MediaMergeRecordRow> for StoredMediaMergeRecord {
    fn from(row: MediaMergeRecordRow) -> Self {
        Self {
            id: row.id,
            library_id: row.library_id,
            item_type: row.item_type,
            primary_item_id: row.primary_item_id,
            merged_item_id: row.merged_item_id,
            merged_item_title: row.merged_item_title,
            origin: row.source,
            merged_at: row.merged_at,
            undone_at: row.undone_at,
            source_count: row.source_count,
        }
    }
}

/// Row of the merge-record lookup at the start of `unmerge_media_items`.
#[derive(Debug, sqlx::FromRow)]
struct MediaMergeRecordKeyRow {
    id: String,
    library_id: String,
    item_type: String,
}

/// Row of the provider keys the two items being unmerged still share.
#[derive(Debug, sqlx::FromRow)]
struct MergeSuppressionProviderKeyRow {
    provider: String,
    provider_id: String,
}

/// Row of the title+year suppression key of the primary item.
///
/// `production_year` is selected with `IS NOT NULL`, so both fields stay non-optional.
#[derive(Debug, sqlx::FromRow)]
struct MergeSuppressionTitleKeyRow {
    sort_title: String,
    production_year: i64,
}

/// Row of `list_provider_identity_merge_keys`: the provider half of a candidate key.
#[derive(Debug, sqlx::FromRow)]
struct ProviderIdentityMergeKeyRow {
    library_id: String,
    item_type: String,
    provider: String,
    provider_id: String,
}

impl From<ProviderIdentityMergeKeyRow> for StoredIdentityMergeCandidateKey {
    fn from(row: ProviderIdentityMergeKeyRow) -> Self {
        Self {
            library_id: row.library_id,
            item_type: row.item_type,
            provider: Some(row.provider),
            provider_id: Some(row.provider_id),
            sort_title: None,
            production_year: None,
        }
    }
}

/// Row of `list_title_identity_merge_keys`: the title+year half of a candidate key.
#[derive(Debug, sqlx::FromRow)]
struct TitleIdentityMergeKeyRow {
    library_id: String,
    item_type: String,
    sort_title: Option<String>,
    production_year: Option<i64>,
}

impl From<TitleIdentityMergeKeyRow> for StoredIdentityMergeCandidateKey {
    fn from(row: TitleIdentityMergeKeyRow) -> Self {
        Self {
            library_id: row.library_id,
            item_type: row.item_type,
            provider: None,
            provider_id: None,
            sort_title: row.sort_title,
            production_year: row.production_year,
        }
    }
}

/// Row of the candidate-group readers, which share one projection.
#[derive(Debug, sqlx::FromRow)]
struct IdentityGroupItemRow {
    id: String,
    added_at: i64,
    source_count: i64,
    default_source_count: i64,
    edition_source_count: i64,
}

impl From<IdentityGroupItemRow> for StoredIdentityMergeCandidateItem {
    fn from(row: IdentityGroupItemRow) -> Self {
        Self {
            id: row.id,
            added_at: row.added_at,
            source_count: row.source_count,
            default_source_count: row.default_source_count,
            edition_source_count: row.edition_source_count,
        }
    }
}

/// Row of `list_identity_group_source_paths`.
#[derive(Debug, sqlx::FromRow)]
struct IdentityGroupSourcePathRow {
    item_id: String,
    relative_path: String,
}

impl Database {
    /// Records one merge record per merged root in a single pass: records, the hierarchy fields a
    /// merge is about to change, and the media sources it is about to move.
    pub(super) async fn record_media_merges_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        item_type: &str,
        primary_item_id: &str,
        merged_item_ids: &[String],
        origin: MediaMergeOrigin,
    ) -> Result<(), StorageError> {
        if merged_item_ids.is_empty() {
            return Ok(());
        }
        let records = merged_item_ids
            .iter()
            .map(|merged_item_id| (Uuid::now_v7().to_string(), merged_item_id.clone()))
            .collect::<Vec<_>>();
        for chunk in records.chunks(MAX_MERGE_RECORD_ROWS_PER_STATEMENT) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, unixepoch())", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO media_merge_records (
                     id, library_id, item_type, primary_item_id, merged_item_id, source, merged_at
                 ) VALUES {values}"
            )));
            for (record_id, merged_item_id) in chunk {
                query = query
                    .bind(record_id)
                    .bind(library_id)
                    .bind(item_type)
                    .bind(primary_item_id)
                    .bind(merged_item_id)
                    .bind(origin.as_sql());
            }
            query
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
        }

        // Seasons and episodes carry the merged series in `series_id`; the root carries itself.
        let mut owners = std::collections::HashMap::<String, String>::new();
        for (record_id, merged_item_id) in &records {
            owners.insert(merged_item_id.clone(), record_id.clone());
        }
        let mut states = Vec::<MergeItemStateRow>::new();
        for chunk in merged_item_ids.chunks(MAX_MERGE_RECORD_ROWS_PER_STATEMENT) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query_as::<MergeItemStateSelectRow>(sqlx::AssertSqlSafe(format!(
                "SELECT id, item_type, parent_id, series_id, merged_into_item_id,
                        CASE WHEN series_id IS NULL THEN id ELSE series_id END AS owner_id
                 FROM media_items
                 WHERE id IN ({placeholders}) OR series_id IN ({placeholders})"
            )));
            for merged_item_id in chunk {
                query = query.bind(merged_item_id);
            }
            for merged_item_id in chunk {
                query = query.bind(merged_item_id);
            }
            for row in query
                .fetch_all(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?
            {
                let Some(record_id) = owners.get(&row.owner_id).cloned() else {
                    continue;
                };
                states.push(MergeItemStateRow {
                    record_id,
                    item_id: row.id,
                    item_type: row.item_type,
                    parent_id: row.parent_id,
                    series_id: row.series_id,
                    merged_into_item_id: row.merged_into_item_id,
                });
            }
        }

        for chunk in states.chunks(MAX_MERGE_RECORD_ROWS_PER_STATEMENT) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO media_merge_item_states (
                     record_id, item_id, item_type, parent_id, series_id, merged_into_item_id
                 ) VALUES {values}"
            )));
            for state in chunk {
                query = query
                    .bind(&state.record_id)
                    .bind(&state.item_id)
                    .bind(&state.item_type)
                    .bind(state.parent_id.as_deref())
                    .bind(state.series_id.as_deref())
                    .bind(state.merged_into_item_id.as_deref());
            }
            query
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
        }

        let mut sources = Vec::<(String, String, String, i64)>::new();
        for chunk in states.chunks(MAX_MERGE_RECORD_ROWS_PER_STATEMENT) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query =
                self.query_as::<MergeSourceStateSelectRow>(sqlx::AssertSqlSafe(format!(
                    "SELECT id, item_id, CAST(is_default AS BIGINT) AS is_default
                 FROM media_sources WHERE item_id IN ({placeholders})"
                )));
            for state in chunk {
                query = query.bind(&state.item_id);
            }
            let record_by_item = chunk
                .iter()
                .map(|state| (state.item_id.clone(), state.record_id.clone()))
                .collect::<std::collections::HashMap<_, _>>();
            for row in query
                .fetch_all(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?
            {
                let Some(record_id) = record_by_item.get(&row.item_id).cloned() else {
                    continue;
                };
                sources.push((record_id, row.id, row.item_id, row.is_default));
            }
        }

        for chunk in sources.chunks(MAX_MERGE_RECORD_ROWS_PER_STATEMENT) {
            let values = std::iter::repeat_n("(?, ?, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO media_merge_source_states (record_id, source_id, item_id, is_default)
                 VALUES {values}"
            )));
            for (record_id, source_id, item_id, is_default) in chunk {
                query = query
                    .bind(record_id)
                    .bind(source_id)
                    .bind(item_id)
                    .bind(is_default);
            }
            query
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
        }
        Ok(())
    }

    pub(crate) async fn list_media_merge_records_for_item(
        &self,
        primary_item_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMediaMergeRecord>, StorageError> {
        let limit = limit.clamp(1, MAX_IDENTITY_MERGE_KEY_PAGE_SIZE);
        self.query_as::<MediaMergeRecordRow>(
            "SELECT record.id, record.library_id, record.item_type,
                    record.primary_item_id, record.merged_item_id,
                    merged_item.title AS merged_item_title, record.source,
                    record.merged_at, record.undone_at,
                    (SELECT COUNT(*) FROM media_merge_source_states state
                     WHERE state.record_id = record.id) AS source_count
             FROM media_merge_records record
             JOIN media_items merged_item ON merged_item.id = record.merged_item_id
             WHERE record.primary_item_id = ?
             ORDER BY record.merged_at DESC, record.id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(primary_item_id)
        .bind(limit)
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(StoredMediaMergeRecord::from).collect())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn unmerge_media_items(
        &self,
        primary_item_id: &str,
        merged_item_id: &str,
    ) -> Result<StoredMediaMergeUndo, StorageError> {
        if primary_item_id == merged_item_id {
            return Err(StorageError::Conflict(
                "主条目和被合并条目不能相同".to_owned(),
            ));
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let record = self
            .query_as::<MediaMergeRecordKeyRow>(
                "SELECT id, library_id, item_type FROM media_merge_records
                 WHERE primary_item_id = ? AND merged_item_id = ? AND undone_at IS NULL
                 ORDER BY merged_at DESC, id DESC
                 LIMIT 1",
            )
            .bind(primary_item_id)
            .bind(merged_item_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some(record) = record else {
            return Err(StorageError::Conflict("没有可撤销的合并记录".to_owned()));
        };
        let record_id = record.id;
        let library_id = record.library_id;
        let item_type = record.item_type;
        let still_merged = self
            .query_scalar::<i64>(
                "SELECT CAST(CASE WHEN EXISTS (
                     SELECT 1 FROM media_items WHERE id = ? AND merged_into_item_id = ?
                 ) THEN 1 ELSE 0 END AS BIGINT)",
            )
            .bind(merged_item_id)
            .bind(primary_item_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if still_merged == 0 {
            return Err(StorageError::Conflict("条目已不处于合并状态".to_owned()));
        }

        // Remember the identity that caused this merge so the background worker cannot merge the
        // same items right back. Provider keys cover TMDb groups; the title+year key covers the
        // fallback for items without provider IDs.
        let shared_provider_keys = self
            .query_as::<MergeSuppressionProviderKeyRow>(
                "SELECT provider, provider_id
                 FROM media_item_provider_ids
                 WHERE media_item_id IN (?, ?)
                 GROUP BY provider, provider_id
                 HAVING COUNT(*) > 1",
            )
            .bind(primary_item_id)
            .bind(merged_item_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if shared_provider_keys.is_empty() {
            let title_key = self
                .query_as::<MergeSuppressionTitleKeyRow>(
                    "SELECT sort_title, production_year FROM media_items
                     WHERE id = ? AND production_year IS NOT NULL",
                )
                .bind(primary_item_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            if let Some(title_key) = title_key {
                self.query(
                    "INSERT INTO merge_suppressions
                         (library_id, item_type, sort_title, production_year)
                     VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
                )
                .bind(&library_id)
                .bind(&item_type)
                .bind(title_key.sort_title)
                .bind(title_key.production_year)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            }
        } else {
            for row in shared_provider_keys {
                self.query(
                    "INSERT INTO merge_suppressions
                         (library_id, item_type, provider, provider_id)
                     VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
                )
                .bind(&library_id)
                .bind(&item_type)
                .bind(row.provider)
                .bind(row.provider_id)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            }
        }

        let item_rows = self
            .query_as::<MergeItemStateRestoreRow>(
                "SELECT item_id, parent_id, series_id, merged_into_item_id
                 FROM media_merge_item_states WHERE record_id = ?",
            )
            .bind(&record_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let mut restored_item_ids = Vec::with_capacity(item_rows.len());
        for chunk in item_rows.chunks(MAX_MERGE_RECORD_ROWS_PER_STATEMENT) {
            let values = std::iter::repeat_n("(?, ?, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let id_placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "WITH merge_map(item_id, parent_id, series_id, merged_into_item_id) AS
                     (VALUES {values})
                 UPDATE media_items
                 SET parent_id = (SELECT parent_id FROM merge_map WHERE item_id = media_items.id),
                     series_id = (SELECT series_id FROM merge_map WHERE item_id = media_items.id),
                     merged_into_item_id = (
                         SELECT merged_into_item_id FROM merge_map WHERE item_id = media_items.id
                     ),
                     updated_at = unixepoch()
                 WHERE id IN ({id_placeholders})"
            )));
            for row in chunk {
                restored_item_ids.push(row.item_id.clone());
                query = query
                    .bind(&row.item_id)
                    .bind(row.parent_id.as_deref())
                    .bind(row.series_id.as_deref())
                    .bind(row.merged_into_item_id.as_deref());
            }
            for row in chunk {
                query = query.bind(&row.item_id);
            }
            query
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
        }

        let source_rows = self
            .query_as::<MergeSourceStateRestoreRow>(
                "SELECT source_id, item_id, CAST(is_default AS BIGINT) AS is_default
                 FROM media_merge_source_states WHERE record_id = ?",
            )
            .bind(&record_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let restored_source_count = source_rows.len();
        let mut normalize_ids = Vec::<String>::new();
        for row in &source_rows {
            if !normalize_ids.contains(&row.item_id) {
                normalize_ids.push(row.item_id.clone());
            }
        }
        for chunk in source_rows.chunks(MAX_MERGE_RECORD_ROWS_PER_STATEMENT) {
            let values = std::iter::repeat_n("(?, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let id_placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "WITH merge_map(source_id, item_id, is_default) AS (VALUES {values})
                 UPDATE media_sources
                 SET item_id = (SELECT item_id FROM merge_map WHERE source_id = media_sources.id),
                     is_default = (
                         SELECT is_default FROM merge_map WHERE source_id = media_sources.id
                     ),
                     updated_at = unixepoch()
                 WHERE id IN ({id_placeholders})"
            )));
            for row in chunk {
                query = query
                    .bind(&row.source_id)
                    .bind(&row.item_id)
                    .bind(row.is_default);
            }
            for row in chunk {
                query = query.bind(&row.source_id);
            }
            query
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
        }

        normalize_ids.push(primary_item_id.to_owned());
        self.normalize_default_sources_in_transaction(&mut transaction, &normalize_ids)
            .await?;
        self.query("UPDATE media_merge_records SET undone_at = unixepoch() WHERE id = ?")
            .bind(&record_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;

        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(StoredMediaMergeUndo {
            library_id,
            item_type,
            primary_item_id: primary_item_id.to_owned(),
            merged_item_id: merged_item_id.to_owned(),
            restored_item_ids,
            restored_source_count,
        })
    }

    pub(crate) async fn list_provider_identity_merge_keys(
        &self,
        after: Option<(&str, &str, &str, &str)>,
        limit: i64,
    ) -> Result<Vec<StoredIdentityMergeCandidateKey>, StorageError> {
        let limit = limit.clamp(1, MAX_IDENTITY_MERGE_KEY_PAGE_SIZE);
        let condition = if after.is_some() {
            "AND (mi.library_id, mi.item_type, provider.provider, provider.provider_id)
                 > (?, ?, ?, ?)"
        } else {
            ""
        };
        let mut query = self.query_as::<ProviderIdentityMergeKeyRow>(sqlx::AssertSqlSafe(format!(
            "SELECT mi.library_id AS library_id, mi.item_type AS item_type,
                    provider.provider AS provider, provider.provider_id AS provider_id
             FROM media_item_provider_ids provider
             JOIN media_items mi ON mi.id = provider.media_item_id
             JOIN libraries library ON library.id = mi.library_id AND library.is_enabled = 1
             WHERE provider.item_type = mi.item_type
               AND mi.item_type IN ('MOVIE', 'SERIES')
               AND mi.merged_into_item_id IS NULL
               AND mi.removed_at IS NULL
               -- Local sidecar NFOs frequently carry stale IMDb/TVDB IDs from another film.
               -- Only TMDb, which Lux resolves itself, may prove two items are the same title.
               AND provider.provider = 'tmdb'
               AND NOT EXISTS (
                   SELECT 1 FROM merge_suppressions suppression
                   WHERE suppression.library_id = mi.library_id
                     AND suppression.item_type = mi.item_type
                     AND suppression.provider = provider.provider
                     AND suppression.provider_id = provider.provider_id
               )
               {condition}
             GROUP BY mi.library_id, mi.item_type, provider.provider, provider.provider_id
             HAVING COUNT(*) > 1
             ORDER BY mi.library_id, mi.item_type, provider.provider, provider.provider_id
             LIMIT ?"
        )));
        if let Some((library_id, item_type, provider, provider_id)) = after {
            query = query
                .bind(library_id)
                .bind(item_type)
                .bind(provider)
                .bind(provider_id);
        }
        query
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(StoredIdentityMergeCandidateKey::from)
                    .collect()
            })
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_title_identity_merge_keys(
        &self,
        after: Option<(&str, &str, &str, i64)>,
        limit: i64,
    ) -> Result<Vec<StoredIdentityMergeCandidateKey>, StorageError> {
        let limit = limit.clamp(1, MAX_IDENTITY_MERGE_KEY_PAGE_SIZE);
        let condition = if after.is_some() {
            "AND (mi.library_id, mi.item_type, mi.sort_title, mi.production_year) > (?, ?, ?, ?)"
        } else {
            ""
        };
        let mut query = self.query_as::<TitleIdentityMergeKeyRow>(sqlx::AssertSqlSafe(format!(
            "SELECT mi.library_id AS library_id, mi.item_type AS item_type,
                    mi.sort_title AS sort_title, mi.production_year AS production_year
             FROM media_items mi
             JOIN libraries library ON library.id = mi.library_id AND library.is_enabled = 1
             WHERE mi.item_type IN ('MOVIE', 'SERIES')
               AND mi.merged_into_item_id IS NULL
               AND mi.removed_at IS NULL
               AND mi.production_year IS NOT NULL
               {condition}
               AND NOT EXISTS (
                   SELECT 1 FROM media_item_provider_ids provider
                   WHERE provider.media_item_id = mi.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM merge_suppressions suppression
                   WHERE suppression.library_id = mi.library_id
                     AND suppression.item_type = mi.item_type
                     AND suppression.sort_title = mi.sort_title
                     AND suppression.production_year = mi.production_year
               )
             GROUP BY mi.library_id, mi.item_type, mi.sort_title, mi.production_year
             HAVING COUNT(*) > 1
             ORDER BY mi.library_id, mi.item_type, mi.sort_title, mi.production_year
             LIMIT ?"
        )));
        if let Some((library_id, item_type, sort_title, production_year)) = after {
            query = query
                .bind(library_id)
                .bind(item_type)
                .bind(sort_title)
                .bind(production_year);
        }
        query
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(StoredIdentityMergeCandidateKey::from)
                    .collect()
            })
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_provider_identity_group_items(
        &self,
        key: &StoredIdentityMergeCandidateKey,
    ) -> Result<Vec<StoredIdentityMergeCandidateItem>, StorageError> {
        let (Some(provider), Some(provider_id)) =
            (key.provider.as_deref(), key.provider_id.as_deref())
        else {
            return Ok(Vec::new());
        };
        self.query_as::<IdentityGroupItemRow>(sqlx::AssertSqlSafe(format!(
            "SELECT mi.id AS id, mi.added_at AS added_at,
                    (SELECT COUNT(*) FROM media_sources source
                     WHERE source.item_id = mi.id) AS source_count,
                    (SELECT COUNT(*) FROM media_sources source
                     WHERE source.item_id = mi.id AND source.is_default = 1)
                        AS default_source_count,
                    {edition_count} AS edition_source_count
             FROM media_item_provider_ids provider
             JOIN media_items mi ON mi.id = provider.media_item_id
             WHERE provider.provider = ? AND provider.provider_id = ?
               AND mi.library_id = ? AND mi.item_type = ?
               AND mi.merged_into_item_id IS NULL
               AND mi.removed_at IS NULL
             ORDER BY mi.id
             LIMIT ?",
            edition_count = EDITION_SOURCE_COUNT_SQL
        )))
        .bind(provider)
        .bind(provider_id)
        .bind(&key.library_id)
        .bind(&key.item_type)
        .bind(MAX_IDENTITY_MERGE_GROUP_ITEMS)
        .fetch_all(&self.pool)
        .await
        .map(stored_identity_group_items)
        .map_err(self.sqlx_err())
    }

    /// Media-source paths for the given candidate items, keyed by item ID.
    ///
    /// Automatic merging uses these first path segments as an identity veto: two items whose
    /// folders name different release years are different titles even when their provider IDs
    /// (usually polluted by a sidecar NFO) agree. Only direct sources are read; series episodes
    /// are intentionally excluded so a multi-season show cannot look like a year conflict.
    pub(crate) async fn list_identity_group_source_paths(
        &self,
        item_ids: &[String],
    ) -> Result<Vec<(String, String)>, StorageError> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let item_ids = item_ids
            .iter()
            .take(MAX_IDENTITY_MERGE_GROUP_ITEMS as usize)
            .collect::<Vec<_>>();
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut query = self.query_as::<IdentityGroupSourcePathRow>(sqlx::AssertSqlSafe(format!(
            "SELECT source.item_id AS item_id, entry.relative_path AS relative_path
             FROM media_sources source
             JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
             WHERE source.item_id IN ({placeholders})
               AND entry.relative_path <> ''
             LIMIT 500"
        )));
        for item_id in item_ids {
            query = query.bind(item_id);
        }
        query
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| (row.item_id, row.relative_path))
                    .collect()
            })
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_title_identity_group_items(
        &self,
        key: &StoredIdentityMergeCandidateKey,
    ) -> Result<Vec<StoredIdentityMergeCandidateItem>, StorageError> {
        let (Some(sort_title), Some(production_year)) =
            (key.sort_title.as_deref(), key.production_year)
        else {
            return Ok(Vec::new());
        };
        self.query_as::<IdentityGroupItemRow>(sqlx::AssertSqlSafe(format!(
            "SELECT mi.id AS id, mi.added_at AS added_at,
                    (SELECT COUNT(*) FROM media_sources source
                     WHERE source.item_id = mi.id) AS source_count,
                    (SELECT COUNT(*) FROM media_sources source
                     WHERE source.item_id = mi.id AND source.is_default = 1)
                        AS default_source_count,
                    {edition_count} AS edition_source_count
             FROM media_items mi
             WHERE mi.library_id = ? AND mi.item_type = ?
               AND mi.sort_title = ? AND mi.production_year = ?
               AND mi.merged_into_item_id IS NULL
               AND mi.removed_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM media_item_provider_ids provider
                   WHERE provider.media_item_id = mi.id
               )
             ORDER BY mi.id
             LIMIT ?",
            edition_count = EDITION_SOURCE_COUNT_SQL
        )))
        .bind(&key.library_id)
        .bind(&key.item_type)
        .bind(sort_title)
        .bind(production_year)
        .bind(MAX_IDENTITY_MERGE_GROUP_ITEMS)
        .fetch_all(&self.pool)
        .await
        .map(stored_identity_group_items)
        .map_err(self.sqlx_err())
    }
}

const EDITION_SOURCE_COUNT_SQL: &str = "CASE WHEN mi.item_type = 'SERIES' THEN (
        SELECT COUNT(*) FROM media_sources episode_source
        JOIN media_items episode ON episode.id = episode_source.item_id
        WHERE episode.series_id = mi.id
          AND episode_source.edition_name IS NOT NULL
          AND episode_source.edition_name <> ''
    ) ELSE (
        SELECT COUNT(*) FROM media_sources source
        WHERE source.item_id = mi.id
          AND source.edition_name IS NOT NULL
          AND source.edition_name <> ''
    ) END";

fn stored_identity_group_items(
    rows: Vec<IdentityGroupItemRow>,
) -> Vec<StoredIdentityMergeCandidateItem> {
    rows.into_iter()
        .map(StoredIdentityMergeCandidateItem::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{application::libraries::LibraryService, config::Config, library::LibraryKind};

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        Ok((temp_dir, database))
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_item(
        database: &Database,
        library_id: &str,
        id: &str,
        item_type: &str,
        parent_id: Option<&str>,
        series_id: Option<&str>,
        season_number: Option<i64>,
        episode_number: Option<i64>,
        provider_ids_json: Option<&str>,
        production_year: Option<i64>,
        title: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, parent_id, series_id, season_number, episode_number,
                 title, sort_title, production_year, provider_ids_json, identification_status
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(id)
        .bind(library_id)
        .bind(item_type)
        .bind(parent_id)
        .bind(series_id)
        .bind(season_number)
        .bind(episode_number)
        .bind(title)
        .bind(title.to_lowercase())
        .bind(production_year)
        .bind(provider_ids_json)
        .execute(database.pool())
        .await
        .map(|_| ())
    }

    async fn insert_source(
        database: &Database,
        source_id: &str,
        item_id: &str,
        edition_name: Option<&str>,
        is_default: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO media_sources (
                 id, item_id, source_kind, edition_name, is_default, probe_status
             ) VALUES (?, ?, 'LOCAL_FILE', ?, ?, 'PENDING')",
        )
        .bind(source_id)
        .bind(item_id)
        .bind(edition_name)
        .bind(is_default)
        .execute(database.pool())
        .await
        .map(|_| ())
    }

    #[tokio::test]
    async fn movie_merge_records_provenance_and_unmerge_restores_sources()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        insert_item(
            &database,
            &library_id,
            "movie-a",
            "MOVIE",
            None,
            None,
            None,
            None,
            None,
            Some(2020),
            "movie-a",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "movie-b",
            "MOVIE",
            None,
            None,
            None,
            None,
            None,
            Some(2021),
            "movie-b",
        )
        .await?;
        insert_source(&database, "source-a", "movie-a", None, 1).await?;
        insert_source(&database, "source-b", "movie-b", None, 1).await?;

        database
            .merge_media_items("movie-a", &["movie-a".to_owned(), "movie-b".to_owned()])
            .await?;

        let records = database
            .list_media_merge_records_for_item("movie-a", 0, 50)
            .await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].merged_item_id, "movie-b");
        assert_eq!(records[0].merged_item_title, "movie-b");
        assert_eq!(records[0].origin, "MANUAL");
        assert_eq!(records[0].source_count, 1);
        assert!(records[0].undone_at.is_none());
        let state_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_merge_item_states WHERE record_id = ?")
                .bind(&records[0].id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(state_count, 1);

        let undo = database.unmerge_media_items("movie-a", "movie-b").await?;
        assert_eq!(undo.restored_source_count, 1);
        assert_eq!(undo.item_type, "MOVIE");
        assert_eq!(undo.library_id, library_id);
        let restored_item: Option<String> =
            sqlx::query_scalar("SELECT item_id FROM media_sources WHERE id = 'source-b'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(restored_item.as_deref(), Some("movie-b"));
        let merged_into: Option<String> =
            sqlx::query_scalar("SELECT merged_into_item_id FROM media_items WHERE id = 'movie-b'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(merged_into, None);
        let is_default: i64 = sqlx::query_scalar(
            "SELECT CAST(is_default AS BIGINT) FROM media_sources WHERE id = 'source-b'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(is_default, 1);
        let undone: Option<i64> =
            sqlx::query_scalar("SELECT undone_at FROM media_merge_records WHERE id = ?")
                .bind(&records[0].id)
                .fetch_one(database.pool())
                .await?;
        assert!(undone.is_some());

        let repeated = database.unmerge_media_items("movie-a", "movie-b").await;
        assert!(matches!(repeated, Err(StorageError::Conflict(_))));
        Ok(())
    }

    #[tokio::test]
    async fn series_merge_records_hierarchy_and_unmerge_restores_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Shows", LibraryKind::Series, false)
            .await?;
        let library_id = library.id.to_string();
        insert_item(
            &database,
            &library_id,
            "series-a",
            "SERIES",
            None,
            None,
            None,
            None,
            None,
            None,
            "series-a",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "series-b",
            "SERIES",
            None,
            None,
            None,
            None,
            None,
            None,
            "series-b",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "season-a1",
            "SEASON",
            Some("series-a"),
            Some("series-a"),
            Some(1),
            None,
            None,
            None,
            "season-a1",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "season-b1",
            "SEASON",
            Some("series-b"),
            Some("series-b"),
            Some(1),
            None,
            None,
            None,
            "season-b1",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "episode-a1",
            "EPISODE",
            Some("season-a1"),
            Some("series-a"),
            Some(1),
            Some(1),
            None,
            None,
            "episode-a1",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "episode-b1",
            "EPISODE",
            Some("season-b1"),
            Some("series-b"),
            Some(1),
            Some(1),
            None,
            None,
            "episode-b1",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "episode-b2",
            "EPISODE",
            Some("season-b1"),
            Some("series-b"),
            Some(1),
            Some(2),
            None,
            None,
            "episode-b2",
        )
        .await?;
        insert_source(&database, "source-a1", "episode-a1", None, 1).await?;
        insert_source(&database, "source-b1", "episode-b1", None, 1).await?;
        insert_source(&database, "source-b2", "episode-b2", None, 1).await?;

        database
            .merge_media_items("series-a", &["series-a".to_owned(), "series-b".to_owned()])
            .await?;
        let merged_episode_sources: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE item_id = 'episode-a1'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(merged_episode_sources, 2);
        let unmatched_episode_series: Option<String> =
            sqlx::query_scalar("SELECT series_id FROM media_items WHERE id = 'episode-b2'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(unmatched_episode_series.as_deref(), Some("series-a"));

        let undo = database.unmerge_media_items("series-a", "series-b").await?;
        assert_eq!(undo.restored_source_count, 2);
        let episode_b1: Option<String> =
            sqlx::query_scalar("SELECT item_id FROM media_sources WHERE id = 'source-b1'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(episode_b1.as_deref(), Some("episode-b1"));
        let season_b1: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT parent_id, merged_into_item_id FROM media_items WHERE id = 'season-b1'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(season_b1, (Some("series-b".to_owned()), None));
        let episode_b2: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT parent_id, series_id FROM media_items WHERE id = 'episode-b2'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(
            episode_b2,
            (Some("season-b1".to_owned()), Some("series-b".to_owned()))
        );
        let series_b: Option<String> =
            sqlx::query_scalar("SELECT merged_into_item_id FROM media_items WHERE id = 'series-b'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(series_b, None);
        Ok(())
    }

    #[tokio::test]
    async fn identity_candidates_group_by_provider_then_title_without_provider_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        insert_item(
            &database,
            &library_id,
            "provider-a",
            "MOVIE",
            None,
            None,
            None,
            None,
            Some(r#"{"tmdb":"42"}"#),
            Some(2020),
            "provider-a",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "provider-b",
            "MOVIE",
            None,
            None,
            None,
            None,
            Some(r#"{"tmdb":"42"}"#),
            Some(2020),
            "provider-b",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "title-a",
            "MOVIE",
            None,
            None,
            None,
            None,
            None,
            Some(1999),
            "Same Title",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "title-b",
            "MOVIE",
            None,
            None,
            None,
            None,
            None,
            Some(1999),
            "Same Title",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "single",
            "MOVIE",
            None,
            None,
            None,
            None,
            None,
            Some(1988),
            "single",
        )
        .await?;
        insert_source(&database, "provider-a-source", "provider-a", None, 1).await?;
        insert_source(
            &database,
            "director-cut",
            "provider-b",
            Some("Director's Cut"),
            1,
        )
        .await?;

        let provider_keys = database.list_provider_identity_merge_keys(None, 50).await?;
        assert_eq!(provider_keys.len(), 1);
        assert_eq!(provider_keys[0].provider.as_deref(), Some("tmdb"));
        assert_eq!(provider_keys[0].provider_id.as_deref(), Some("42"));

        let title_keys = database.list_title_identity_merge_keys(None, 50).await?;
        assert_eq!(title_keys.len(), 1);
        assert_eq!(title_keys[0].sort_title.as_deref(), Some("same title"));
        assert_eq!(title_keys[0].production_year, Some(1999));

        let group_items = database
            .list_provider_identity_group_items(&provider_keys[0])
            .await?;
        assert_eq!(group_items.len(), 2);
        let edition_flags = group_items
            .iter()
            .map(|item| (item.id.as_str(), item.edition_source_count))
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(edition_flags.get("provider-a"), Some(&0));
        assert_eq!(edition_flags.get("provider-b"), Some(&1));
        Ok(())
    }

    #[tokio::test]
    async fn title_identity_group_items_decode_source_and_edition_counts()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        // Both items lack provider IDs, so they only fall into the title+year fallback group.
        insert_item(
            &database,
            &library_id,
            "plain-a",
            "MOVIE",
            None,
            None,
            None,
            None,
            None,
            Some(1999),
            "Same Title",
        )
        .await?;
        insert_item(
            &database,
            &library_id,
            "plain-b",
            "MOVIE",
            None,
            None,
            None,
            None,
            None,
            Some(1999),
            "Same Title",
        )
        .await?;
        insert_source(&database, "plain-a-main", "plain-a", None, 1).await?;
        insert_source(&database, "plain-a-extra", "plain-a", None, 0).await?;
        insert_source(
            &database,
            "plain-b-cut",
            "plain-b",
            Some("Director's Cut"),
            1,
        )
        .await?;

        let key = StoredIdentityMergeCandidateKey {
            library_id: library_id.clone(),
            item_type: "MOVIE".to_owned(),
            provider: None,
            provider_id: None,
            sort_title: Some("same title".to_owned()),
            production_year: Some(1999),
        };
        let items = database.list_title_identity_group_items(&key).await?;
        let decoded = items
            .iter()
            .map(|item| {
                (
                    item.id.as_str(),
                    item.source_count,
                    item.default_source_count,
                    item.edition_source_count,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(decoded, vec![("plain-a", 2, 1, 0), ("plain-b", 1, 1, 1)]);
        assert!(items.iter().all(|item| item.added_at > 0));
        Ok(())
    }

    #[tokio::test]
    async fn identity_group_source_paths_decode_item_and_relative_path()
    -> Result<(), Box<dyn std::error::Error>> {
        let (temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        insert_item(
            &database,
            &library_id,
            "path-item",
            "MOVIE",
            None,
            None,
            None,
            None,
            None,
            Some(2010),
            "path-item",
        )
        .await?;
        let root_path = temp_dir.path().to_string_lossy().into_owned();
        sqlx::query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('path-root', ?, ?, ?, 1, 1)",
        )
        .bind(&library_id)
        .bind(&root_path)
        .bind(&root_path)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation
             ) VALUES
                ('path-movie-file', 'path-root', 'Movies (2010)/path.mkv', 'FILE', 10, 1,
                 'generation'),
                ('path-blank-file', 'path-root', '', 'DIRECTORY', 0, 1, 'generation')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_sources (
                 id, item_id, source_kind, filesystem_entry_id, is_default, probe_status
             ) VALUES
                ('path-movie-source', 'path-item', 'LOCAL_FILE', 'path-movie-file', 1, 'READY'),
                ('path-blank-source', 'path-item', 'LOCAL_FILE', 'path-blank-file', 0, 'READY')",
        )
        .execute(database.pool())
        .await?;

        let paths = database
            .list_identity_group_source_paths(&["path-item".to_owned()])
            .await?;
        // The blank relative path is filtered by the query, not by the decoder.
        assert_eq!(
            paths,
            vec![("path-item".to_owned(), "Movies (2010)/path.mkv".to_owned())]
        );
        Ok(())
    }

    #[tokio::test]
    async fn unmerge_records_the_provider_suppression_shared_by_both_items()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        for (id, year) in [("movie-a", 2020), ("movie-b", 2021)] {
            insert_item(
                &database,
                &library_id,
                id,
                "MOVIE",
                None,
                None,
                None,
                None,
                Some(r#"{"tmdb":"42"}"#),
                Some(year),
                id,
            )
            .await?;
            insert_source(&database, &format!("source-{id}"), id, None, 1).await?;
        }
        // The media_items trigger indexes each item's provider_ids_json, so both items now carry
        // the same `tmdb`/`42` key: the unmerge must remember that provider key instead of falling
        // back to the title+year suppression.
        let indexed_keys: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_item_provider_ids
             WHERE provider = 'tmdb' AND provider_id = '42'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(indexed_keys, 2);

        database
            .merge_media_items("movie-a", &["movie-a".to_owned(), "movie-b".to_owned()])
            .await?;
        database.unmerge_media_items("movie-a", "movie-b").await?;

        let suppressions = sqlx::query_as::<_, (String, String)>(
            "SELECT provider, provider_id FROM merge_suppressions
             WHERE library_id = ? AND item_type = 'MOVIE'",
        )
        .bind(&library_id)
        .fetch_all(database.pool())
        .await?;
        // A single provider row: the shared key branch ran and the title fallback did not.
        assert_eq!(suppressions, vec![("tmdb".to_owned(), "42".to_owned())]);
        Ok(())
    }

    #[tokio::test]
    async fn media_merge_record_row_reports_a_missing_column_instead_of_panicking()
    -> Result<(), Box<dyn std::error::Error>> {
        use sqlx::FromRow;

        let (_temp_dir, database) = test_database().await?;
        let row = sqlx::query("SELECT 1 AS other")
            .fetch_one(database.pool())
            .await?;
        let error =
            MediaMergeRecordRow::from_row(&row).expect_err("missing columns must not panic");
        assert!(
            matches!(error, sqlx::Error::ColumnNotFound(_)),
            "unexpected decode error: {error}"
        );
        Ok(())
    }
}
