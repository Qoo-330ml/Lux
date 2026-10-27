//! Read-model fan-out primitives: append-only outbox plus per-consumer watermarks.

use super::*;

#[allow(dead_code)] // LUX-864 wires the consumer.
#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct CatalogOutboxEntry {
    pub(crate) id: i64,
    pub(crate) kind: String,
    pub(crate) item_id: String,
    pub(crate) library_id: Option<String>,
}

#[allow(dead_code)] // LUX-864 wires the consumer.
#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct CatalogConsumerState {
    pub(crate) name: String,
    pub(crate) applied_seq: i64,
    pub(crate) updated_at: i64,
    pub(crate) last_error: Option<String>,
}

#[allow(dead_code)] // LUX-864 wires the consumer.
impl Database {
    /// Appends one derived-data event and returns its sequence number.
    pub(crate) async fn append_catalog_outbox(
        &self,
        kind: &str,
        item_id: &str,
        library_id: Option<&str>,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "INSERT INTO catalog_outbox (kind, item_id, library_id)
             VALUES (?, ?, ?)
             RETURNING id",
        )
        .bind(kind)
        .bind(item_id)
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Reads at most `limit` events after `after_seq`, oldest first.
    pub(crate) async fn read_catalog_outbox_after(
        &self,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<CatalogOutboxEntry>, StorageError> {
        self.query_as(
            "SELECT id, kind, item_id, library_id
             FROM catalog_outbox
             WHERE id > ?
             ORDER BY id
             LIMIT ?",
        )
        .bind(after_seq)
        .bind(limit.clamp(1, 5_000))
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn catalog_consumer(
        &self,
        name: &str,
    ) -> Result<Option<CatalogConsumerState>, StorageError> {
        self.query_as(
            "SELECT name, applied_seq, updated_at, last_error
             FROM catalog_consumers WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Advances a consumer watermark. The watermark never moves backwards, so a retried batch is
    /// idempotent.
    pub(crate) async fn advance_catalog_consumer(
        &self,
        name: &str,
        applied_seq: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO catalog_consumers (name, applied_seq)
             VALUES (?, ?)
             ON CONFLICT(name) DO UPDATE SET
                 applied_seq = excluded.applied_seq,
                 updated_at = unixepoch(),
                 last_error = NULL
             WHERE catalog_consumers.applied_seq < excluded.applied_seq",
        )
        .bind(name)
        .bind(applied_seq)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn record_catalog_consumer_error(
        &self,
        name: &str,
        error: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO catalog_consumers (name, last_error)
             VALUES (?, ?)
             ON CONFLICT(name) DO UPDATE SET
                 updated_at = unixepoch(),
                 last_error = excluded.last_error",
        )
        .bind(name)
        .bind(error)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Detects that the queue was trimmed past a consumer's watermark.
    ///
    /// Returns `(applied_seq, oldest_remaining_seq)` when the next expected sequence is gone, so the
    /// caller can schedule a full rebuild instead of silently skipping derived rows.
    pub(crate) async fn catalog_outbox_gap(
        &self,
        name: &str,
    ) -> Result<Option<(i64, i64)>, StorageError> {
        let applied_seq = self
            .catalog_consumer(name)
            .await?
            .map(|state| state.applied_seq)
            .unwrap_or(0);
        let oldest = self
            .query_scalar::<Option<i64>>("SELECT MIN(id) FROM catalog_outbox")
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .flatten();
        Ok(oldest.and_then(|oldest| {
            (oldest > applied_seq.saturating_add(1)).then_some((applied_seq, oldest))
        }))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct CatalogReadModelSummary {
    pub(crate) item_count: i64,
    pub(crate) content_sha256: String,
}

impl Database {
    #[allow(dead_code)] // used by rebuild tests and diagnostics
    pub(crate) async fn catalog_items_count(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM catalog_items")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    /// Highest item id already materialized, used to resume an interrupted rebuild.
    pub(crate) async fn catalog_items_max_item_id(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar("SELECT MAX(item_id) FROM catalog_items")
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.flatten())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    /// Rebuilds one keyset page of `catalog_items` from the authoritative tables.
    ///
    /// Returns `(rows_processed, last_item_id)`; the caller resumes with `after = last_item_id`.
    pub(crate) async fn rebuild_catalog_items_batch(
        &self,
        after_item_id: Option<&str>,
        limit: i64,
    ) -> Result<(u64, Option<String>), StorageError> {
        let limit = limit.clamp(1, 5_000);
        let after = after_item_id.unwrap_or("");
        let rows: Vec<String> = self
            .query_scalar(
                "SELECT id FROM media_items
                 WHERE id > ?
                 ORDER BY id
                 LIMIT ?",
            )
            .bind(after)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if rows.is_empty() {
            return Ok((0, None));
        }
        let last = rows.last().cloned();
        let upsert = self
            .query(
                "INSERT INTO catalog_items (
                     item_id, library_id, item_type, parent_id, series_id, season_number,
                     episode_number, title, sort_title, original_title, production_year,
                     rating, runtime_ticks, added_at, effective_added_at, updated_at,
                     removed_at, merged_into, has_available_source, default_source_id,
                     poster_tag, backdrop_tag, thumb_tag, logo_tag, season_count,
                     episode_count, latest_child_added_at, restriction_code, metadata_state,
                     nfo_fingerprint, applied_seq
                 )
                 SELECT
                     mi.id, mi.library_id, mi.item_type, mi.parent_id, mi.series_id,
                     mi.season_number, mi.episode_number, mi.title, mi.sort_title,
                     mi.original_title, mi.production_year, mi.rating, mi.runtime_ticks,
                     mi.added_at, mi.added_at, mi.updated_at, mi.removed_at,
                     mi.merged_into_item_id, mi.has_available_source,
                     (SELECT ms.id FROM media_sources ms WHERE ms.item_id = mi.id
                       ORDER BY ms.is_default DESC, ms.id LIMIT 1),
                     (SELECT ii.id FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'POSTER'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.id FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'FANART'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.id FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'THUMB'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.id FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'LOGO'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT COUNT(*) FROM media_items child
                       WHERE child.series_id = mi.id AND child.item_type = 'SEASON'),
                     (SELECT COUNT(*) FROM media_items child
                       WHERE child.item_type = 'EPISODE'
                         AND (child.series_id = mi.id OR child.parent_id = mi.id)),
                     (SELECT MAX(child.added_at) FROM media_items child
                       WHERE child.parent_id = mi.id OR child.series_id = mi.id),
                     (SELECT cri.certification_code FROM media_content_restriction_index cri
                       WHERE cri.item_id = mi.id LIMIT 1),
                     mi.identification_status, mi.nfo_metadata_fingerprint, 0
                 FROM media_items mi
                 WHERE mi.id > ? AND mi.id <= ?
                 ON CONFLICT(item_id) DO UPDATE SET
                     library_id = excluded.library_id,
                     item_type = excluded.item_type,
                     parent_id = excluded.parent_id,
                     series_id = excluded.series_id,
                     season_number = excluded.season_number,
                     episode_number = excluded.episode_number,
                     title = excluded.title,
                     sort_title = excluded.sort_title,
                     original_title = excluded.original_title,
                     production_year = excluded.production_year,
                     rating = excluded.rating,
                     runtime_ticks = excluded.runtime_ticks,
                     added_at = excluded.added_at,
                     effective_added_at = excluded.effective_added_at,
                     updated_at = excluded.updated_at,
                     removed_at = excluded.removed_at,
                     merged_into = excluded.merged_into,
                     has_available_source = excluded.has_available_source,
                     default_source_id = excluded.default_source_id,
                     poster_tag = excluded.poster_tag,
                     backdrop_tag = excluded.backdrop_tag,
                     thumb_tag = excluded.thumb_tag,
                     logo_tag = excluded.logo_tag,
                     season_count = excluded.season_count,
                     episode_count = excluded.episode_count,
                     latest_child_added_at = excluded.latest_child_added_at,
                     restriction_code = excluded.restriction_code,
                     metadata_state = excluded.metadata_state,
                     nfo_fingerprint = excluded.nfo_fingerprint,
                     applied_seq = excluded.applied_seq",
            )
            .bind(after)
            .bind(last.as_deref())
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((upsert.rows_affected(), last))
    }

    pub(crate) async fn rebuild_catalog_item_stats(&self) -> Result<(), StorageError> {
        self.query("DELETE FROM catalog_item_stats")
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO catalog_item_stats (scope, scope_id, item_type, total)
             SELECT 'library', library_id, item_type, COUNT(*)
             FROM catalog_items WHERE removed_at IS NULL
             GROUP BY library_id, item_type",
        )
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "INSERT INTO catalog_item_stats (scope, scope_id, item_type, total)
             SELECT 'global', '', item_type, COUNT(*)
             FROM catalog_items WHERE removed_at IS NULL
             GROUP BY item_type",
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[allow(dead_code)] // used by the read-model reset path in LUX-865
    pub(crate) async fn truncate_catalog_read_model(&self) -> Result<(), StorageError> {
        self.query("DELETE FROM catalog_item_stats")
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM catalog_items")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    /// Stable hash of the read model's user-visible columns, used by rebuild validation.
    pub(crate) async fn catalog_items_checksum(
        &self,
    ) -> Result<CatalogReadModelSummary, StorageError> {
        use sha2::{Digest, Sha256};

        let rows: Vec<(String, String, i64, i64)> = self
            .query_as(
                "SELECT item_id, sort_title, effective_added_at, has_available_source
                 FROM catalog_items ORDER BY item_id",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut hasher = Sha256::new();
        for (item_id, sort_title, added, available) in &rows {
            hasher.update(item_id.as_bytes());
            hasher.update(b"\0");
            hasher.update(sort_title.as_bytes());
            hasher.update(b"\0");
            hasher.update(added.to_string().as_bytes());
            hasher.update(b"\0");
            hasher.update(available.to_string().as_bytes());
            hasher.update(b"\n");
        }
        Ok(CatalogReadModelSummary {
            item_count: i64::try_from(rows.len()).unwrap_or(i64::MAX),
            content_sha256: format!("{:x}", hasher.finalize()),
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ReadModelSort {
    SortTitle,
    EffectiveAddedAt,
    Rating,
    ProductionYear,
}

pub(crate) struct ReadModelPageQuery<'a> {
    pub(crate) library_id: &'a str,
    pub(crate) item_types: &'a [String],
    pub(crate) excluded_item_types: &'a [String],
    pub(crate) sort: ReadModelSort,
    pub(crate) descending: bool,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
    pub(crate) include_total: bool,
}

impl Database {
    /// Shadow query for the default library list: ids and total only, so it can be compared with
    /// the authoritative path without changing the response.
    #[allow(dead_code)] // wired by the shadow comparison in the catalog service
    pub(crate) async fn shadow_catalog_library_page(
        &self,
        page: &ReadModelPageQuery<'_>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let ReadModelPageQuery {
            library_id,
            item_types,
            excluded_item_types,
            sort,
            descending,
            offset,
            limit,
            include_total,
        } = *page;
        let order = match sort {
            ReadModelSort::SortTitle => "sort_title",
            ReadModelSort::EffectiveAddedAt => "effective_added_at",
            ReadModelSort::Rating => "rating",
            ReadModelSort::ProductionYear => "production_year",
        };
        let direction = if descending { "DESC" } else { "ASC" };
        let mut conditions = vec![
            "library_id = ?".to_owned(),
            "removed_at IS NULL".to_owned(),
            "merged_into IS NULL".to_owned(),
            "has_available_source = 1".to_owned(),
        ];
        if !item_types.is_empty() {
            let placeholders = std::iter::repeat_n("?", item_types.len())
                .collect::<Vec<_>>()
                .join(", ");
            conditions.push(format!("item_type IN ({placeholders})"));
        }
        if !excluded_item_types.is_empty() {
            let placeholders = std::iter::repeat_n("?", excluded_item_types.len())
                .collect::<Vec<_>>()
                .join(", ");
            conditions.push(format!("item_type NOT IN ({placeholders})"));
        }
        let where_clause = conditions.join(" AND ");
        let list_query = format!(
            "SELECT item_id FROM catalog_items
             WHERE {where_clause}
             ORDER BY {order} {direction}, item_id
             LIMIT ? OFFSET ?"
        );
        let mut statement = self
            .query_as::<(String,)>(sqlx::AssertSqlSafe(list_query))
            .bind(library_id);
        for item_type in item_types {
            statement = statement.bind(item_type);
        }
        for item_type in excluded_item_types {
            statement = statement.bind(item_type);
        }
        let ids = statement
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(|(id,)| id)
            .collect::<Vec<_>>();
        let total = if include_total {
            let count_query = format!("SELECT COUNT(*) FROM catalog_items WHERE {where_clause}");
            let mut count_statement = self
                .query_scalar::<i64>(sqlx::AssertSqlSafe(count_query))
                .bind(library_id);
            for item_type in item_types {
                count_statement = count_statement.bind(item_type);
            }
            for item_type in excluded_item_types {
                count_statement = count_statement.bind(item_type);
            }
            count_statement
                .fetch_one(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
        } else {
            0
        };
        Ok((ids, total))
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ReadModelCoreRow {
    item_id: String,
    library_id: String,
    item_type: String,
    parent_id: Option<String>,
    series_id: Option<String>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    title: String,
    sort_title: String,
    original_title: Option<String>,
    overview: Option<String>,
    rating_source: Option<String>,
    production_year: Option<i64>,
    rating: Option<f64>,
    runtime_ticks: Option<i64>,
    added_at: i64,
    updated_at: i64,
    poster_tag: Option<String>,
    backdrop_tag: Option<String>,
    thumb_tag: Option<String>,
    logo_tag: Option<String>,
    series_name: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ReadModelSourceRow {
    item_id: String,
    source_relative_path: Option<String>,
    source_id: Option<String>,
    source_kind: Option<String>,
    container: Option<String>,
    size: Option<i64>,
    external_url: Option<String>,
    edition_name: Option<String>,
    quality_label: Option<String>,
    bitrate: Option<i64>,
    duration_ticks: Option<i64>,
    is_default: Option<i64>,
    probe_status: Option<String>,
    stream_id: Option<String>,
    stream_index: Option<i64>,
    stream_type: Option<String>,
    codec: Option<String>,
    language: Option<String>,
    stream_title: Option<String>,
    stream_details_json: Option<String>,
    stream_is_external: Option<i64>,
    stream_is_default: Option<i64>,
    stream_is_forced: Option<i64>,
}

impl Database {
    /// Builds the legacy row shape from `catalog_items` plus one batched source/stream query.
    #[allow(dead_code)] // wired by the `on` mode switch
    pub(crate) async fn list_catalog_rows_from_read_model(
        &self,
        page: &ReadModelPageQuery<'_>,
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        let (ids, total) = self.shadow_catalog_library_page(page).await?;
        let rows = self.read_model_rows_for_ids(&ids).await?;
        Ok((rows, total))
    }

    /// Single-item variant used by the detail route.
    #[allow(dead_code)] // wired by the `on` detail switch
    pub(crate) async fn find_catalog_rows_from_read_model(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        self.read_model_rows_for_ids(&[item_id.to_owned()]).await
    }

    /// Same as [`Self::read_model_rows_for_ids`] but keeps the requested id order, which derived
    /// lists (continue watching) need to preserve their authoritative ordering.
    pub(crate) async fn read_model_rows_in_id_order(
        &self,
        ids: &[String],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let mut rows = self.read_model_rows_for_ids(ids).await?;
        let position = ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect::<std::collections::HashMap<_, _>>();
        rows.sort_by_key(|row| {
            position
                .get(row.item_id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        Ok(rows)
    }

    async fn read_model_rows_for_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let core_query = format!(
            "SELECT ci.item_id, ci.library_id, ci.item_type, ci.parent_id, ci.series_id,
                    ci.season_number, ci.episode_number, ci.title, ci.sort_title,
                    ci.original_title, mi.overview, mi.rating_source,
                    ci.production_year, ci.rating, ci.runtime_ticks,
                    ci.added_at, ci.updated_at, ci.poster_tag, ci.backdrop_tag,
                    ci.thumb_tag, ci.logo_tag,
                    (SELECT s.title FROM media_items s WHERE s.id = ci.series_id) AS series_name
             FROM catalog_items ci
             JOIN media_items mi ON mi.id = ci.item_id
             WHERE ci.item_id IN ({placeholders})"
        );
        let mut statement = self.query_as::<ReadModelCoreRow>(sqlx::AssertSqlSafe(core_query));
        for id in ids {
            statement = statement.bind(id);
        }
        let cores = statement
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let source_query = format!(
            "SELECT ms.item_id,
                    (SELECT fe.relative_path FROM filesystem_entries fe
                     WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size,
                    ms.external_url, ms.edition_name, ms.quality_label, ms.bitrate,
                    ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type, mt.codec,
                    mt.language, mt.title AS stream_title, mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external, mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM media_sources ms
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE ms.item_id IN ({placeholders})
               AND EXISTS (
                   SELECT 1 FROM filesystem_entries fe
                   WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
               )
             ORDER BY ms.item_id, ms.id, mt.stream_index"
        );
        let mut statement = self.query_as::<ReadModelSourceRow>(sqlx::AssertSqlSafe(source_query));
        for id in ids {
            statement = statement.bind(id);
        }
        let sources =
            statement
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        let mut by_item: std::collections::HashMap<String, Vec<ReadModelSourceRow>> =
            std::collections::HashMap::new();
        for source in sources {
            by_item
                .entry(source.item_id.clone())
                .or_default()
                .push(source);
        }
        let mut rows = Vec::new();
        for core in cores {
            let item_sources = by_item.remove(&core.item_id).unwrap_or_default();
            if item_sources.is_empty() {
                rows.push(core_row_without_source(&core));
                continue;
            }
            for source in item_sources {
                rows.push(core_row_with_source(&core, &source));
            }
        }
        Ok(rows)
    }
}

fn core_row_without_source(core: &ReadModelCoreRow) -> StoredCatalogRow {
    StoredCatalogRow {
        item_id: core.item_id.clone(),
        library_id: core.library_id.clone(),
        item_type: core.item_type.clone(),
        parent_id: core.parent_id.clone(),
        series_id: core.series_id.clone(),
        series_name: core.series_name.clone(),
        season_number: core.season_number,
        episode_number: core.episode_number,
        title: core.title.clone(),
        sort_title: core.sort_title.clone(),
        original_title: core.original_title.clone(),
        overview: core.overview.clone(),
        production_year: core.production_year,
        rating: core.rating,
        rating_source: core.rating_source.clone(),
        runtime_ticks: core.runtime_ticks,
        added_at: core.added_at,
        updated_at: core.updated_at,
        poster_image_tag: core.poster_tag.clone(),
        fanart_image_tag: core.backdrop_tag.clone(),
        thumb_image_tag: core.thumb_tag.clone(),
        logo_image_tag: core.logo_tag.clone(),
        source_id: None,
        source_relative_path: None,
        source_kind: None,
        container: None,
        size: None,
        external_url: None,
        edition_name: None,
        quality_label: None,
        bitrate: None,
        duration_ticks: None,
        is_default: None,
        probe_status: None,
        stream_id: None,
        stream_index: None,
        stream_type: None,
        codec: None,
        language: None,
        stream_title: None,
        stream_details_json: None,
        stream_is_external: None,
        stream_is_default: None,
        stream_is_forced: None,
    }
}

fn core_row_with_source(core: &ReadModelCoreRow, source: &ReadModelSourceRow) -> StoredCatalogRow {
    let mut row = core_row_without_source(core);
    row.source_id = source.source_id.clone();
    row.source_relative_path = source.source_relative_path.clone();
    row.source_kind = source.source_kind.clone();
    row.container = source.container.clone();
    row.size = source.size;
    row.external_url = source.external_url.clone();
    row.edition_name = source.edition_name.clone();
    row.quality_label = source.quality_label.clone();
    row.bitrate = source.bitrate;
    row.duration_ticks = source.duration_ticks;
    row.is_default = source.is_default.map(|value| value != 0);
    row.probe_status = source.probe_status.clone();
    row.stream_id = source.stream_id.clone();
    row.stream_index = source.stream_index;
    row.stream_type = source.stream_type.clone();
    row.codec = source.codec.clone();
    row.language = source.language.clone();
    row.stream_title = source.stream_title.clone();
    row.stream_details_json = source.stream_details_json.clone();
    row.stream_is_external = source.stream_is_external.map(|value| value != 0);
    row.stream_is_default = source.stream_is_default.map(|value| value != 0);
    row.stream_is_forced = source.stream_is_forced.map(|value| value != 0);
    row
}
