use super::*;

struct LibraryTaskPlanAssignment {
    id: String,
    task_name: String,
    task_description: String,
    source_type: String,
    plugin_id: Option<String>,
    schedule: Option<String>,
    is_enabled: bool,
    resource_limit_json: String,
}

impl Database {
    async fn ensure_library_task_plan(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        task_type: &str,
        task_name: &str,
        task_description: &str,
        source_type: &str,
        plugin_id: Option<&str>,
        schedule: Option<&str>,
        is_enabled: bool,
        resource_limit_json: &str,
    ) -> Result<LibraryTaskPlanAssignment, StorageError> {
        let default_plan: Option<(
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            String,
        )> = self
            .query_as(
                "SELECT id, plan_name, task_name, task_description, source_type,
                        plugin_id, cron_or_interval, is_enabled, resource_limit_json
                 FROM scheduled_task_plans
                 WHERE scope_type = 'LIBRARY' AND task_type = ? AND is_default = 1
                   AND source_type = ?
                   AND (plugin_id = ? OR (plugin_id IS NULL AND ? IS NULL))
                 ORDER BY updated_at, id
                 LIMIT 1",
            )
            .bind(task_type)
            .bind(source_type)
            .bind(plugin_id)
            .bind(plugin_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        if let Some((
            id,
            _plan_name,
            task_name,
            task_description,
            source_type,
            plugin_id,
            schedule,
            enabled,
            resource_limit_json,
        )) = default_plan
        {
            return Ok(LibraryTaskPlanAssignment {
                id,
                task_name,
                task_description,
                source_type,
                plugin_id,
                schedule,
                is_enabled: enabled != 0,
                resource_limit_json,
            });
        }

        let id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO scheduled_task_plans (
                id, task_type, plan_name, task_name, task_description,
                source_type, plugin_id, cron_or_interval, is_enabled,
                resource_limit_json, scope_type, is_default
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'LIBRARY', 1)",
        )
        .bind(&id)
        .bind(task_type)
        .bind(task_name)
        .bind(task_name)
        .bind(task_description)
        .bind(source_type)
        .bind(plugin_id)
        .bind(schedule)
        .bind(database_flag(is_enabled))
        .bind(resource_limit_json)
        .execute(&mut **transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        Ok(LibraryTaskPlanAssignment {
            id,
            task_name: task_name.to_owned(),
            task_description: task_description.to_owned(),
            source_type: source_type.to_owned(),
            plugin_id: plugin_id.map(str::to_owned),
            schedule: schedule.map(str::to_owned),
            is_enabled,
            resource_limit_json: resource_limit_json.to_owned(),
        })
    }

    pub(crate) async fn insert_library(&self, library: NewLibrary<'_>) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO libraries (
                id, name, kind, is_enabled, realtime_watch_enabled,
                realtime_metadata_auto_match_enabled,
                reconciliation_schedule, metadata_schedule,
                scan_concurrency, probe_concurrency, scraper_id, chapter_source_id
            ) VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(library.id)
        .bind(library.name)
        .bind(library.kind)
        .bind(database_flag(library.realtime_watch_enabled))
        .bind(database_flag(library.realtime_metadata_auto_match_enabled))
        .bind(library.reconciliation_schedule)
        .bind(library.metadata_schedule)
        .bind(library.scan_concurrency)
        .bind(library.probe_concurrency)
        .bind(library.scraper_id)
        .bind(library.chapter_source_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        for scraper in library.scrapers {
            self.query(
                "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(library.id)
            .bind(&scraper.scraper_id)
            .bind(scraper.position)
            .bind(scraper.role.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }

        let registrations = [
            (
                "RECONCILIATION_SCAN",
                "全量校验媒体库",
                "按计划校验媒体库索引与文件系统的一致性。",
                "SYSTEM",
                None,
                library.reconciliation_schedule,
            ),
            (
                "METADATA_PARSE",
                "元数据刮削",
                "解析本地元数据，并在已配置时调用刮削插件补全内容。",
                if library.scraper_id.is_some() {
                    "PLUGIN"
                } else {
                    "SYSTEM"
                },
                library.scraper_id,
                library.metadata_schedule,
            ),
        ];
        for (task_type, task_name, task_description, source_type, plugin_id, schedule) in
            registrations
        {
            let resource_limit_json = if task_type == "RECONCILIATION_SCAN" {
                format!(
                    "{{\"scanConcurrency\":{},\"probeConcurrency\":{}}}",
                    library.scan_concurrency, library.probe_concurrency
                )
            } else {
                "{}".to_owned()
            };
            let assignment = self
                .ensure_library_task_plan(
                    &mut transaction,
                    task_type,
                    task_name,
                    task_description,
                    source_type,
                    plugin_id,
                    schedule,
                    schedule.is_some(),
                    &resource_limit_json,
                )
                .await?;
            self.query(
                "INSERT INTO scheduled_task_plan_libraries (plan_id, library_id)
                 VALUES (?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(&assignment.id)
            .bind(library.id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "INSERT INTO scheduled_task_configs (
                    owner_type, owner_id, task_type, task_name, task_description,
                    source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json,
                    plan_id
                ) VALUES ('LIBRARY', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(library.id)
            .bind(task_type)
            .bind(&assignment.task_name)
            .bind(&assignment.task_description)
            .bind(&assignment.source_type)
            .bind(assignment.plugin_id.as_deref())
            .bind(assignment.schedule.as_deref())
            .bind(database_flag(assignment.is_enabled))
            .bind(&assignment.resource_limit_json)
            .bind(&assignment.id)
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

    pub(crate) async fn person_index_item_state_is_current(
        &self,
        item_id: &str,
        source_fingerprint: Option<&str>,
    ) -> Result<bool, StorageError> {
        let Some(source_fingerprint) = source_fingerprint else {
            return Ok(false);
        };
        let row = self
            .query(
                "SELECT source_fingerprint, relation_schema_version
                 FROM person_index_item_state WHERE item_id = ?",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(row.is_some_and(|row| {
            row.get::<Option<String>, _>("source_fingerprint")
                .as_deref()
                == Some(source_fingerprint)
                && row.get::<i64, _>("relation_schema_version") == 2
        }))
    }

    pub(crate) async fn list_library_scrapers(
        &self,
        library_id: &str,
    ) -> Result<Vec<StoredLibraryScraper>, StorageError> {
        self.query(
            "SELECT scraper_id, position, role
             FROM library_scrapers
             WHERE library_id = ?
             ORDER BY position",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredLibraryScraper {
                    scraper_id: row.get("scraper_id"),
                    position: row.get("position"),
                    role: row.get("role"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_library_scrapers_by_library_ids(
        &self,
        library_ids: &[String],
    ) -> Result<HashMap<String, Vec<StoredLibraryScraper>>, StorageError> {
        if library_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut scrapers = HashMap::<String, Vec<StoredLibraryScraper>>::new();
        for library_ids in library_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", library_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT library_id, scraper_id, position, role
                 FROM library_scrapers
                 WHERE library_id IN ({placeholders})
                 ORDER BY library_id, position"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for library_id in library_ids {
                statement = statement.bind(library_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in rows {
                let library_id: String = row.get("library_id");
                scrapers
                    .entry(library_id)
                    .or_default()
                    .push(StoredLibraryScraper {
                        scraper_id: row.get("scraper_id"),
                        position: row.get("position"),
                        role: row.get("role"),
                    });
            }
        }
        Ok(scrapers)
    }

    pub(crate) async fn list_libraries(&self) -> Result<Vec<StoredLibrary>, StorageError> {
        let rows = self
            .query(
            "SELECT id, name, kind, is_enabled, realtime_watch_enabled,
                    realtime_metadata_auto_match_enabled,
                    incremental_schedule, reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, last_scan_at, scraper_id, chapter_source_id,
                    cover_image_path, cover_image_content_type, cover_image_size, cover_image_tag,
                    media_strategy_json
             FROM libraries ORDER BY name, id",
                )
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        let library_ids = rows
            .iter()
            .map(|row| row.get::<String, _>("id"))
            .collect::<Vec<_>>();
        let mut scrapers = self
            .list_library_scrapers_by_library_ids(&library_ids)
            .await?;
        let mut libraries = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.get("id");
            libraries.push(StoredLibrary {
                id: id.clone(),
                name: row.get("name"),
                kind: row.get("kind"),
                is_enabled: row.get::<i64, _>("is_enabled") != 0,
                realtime_watch_enabled: row.get::<i64, _>("realtime_watch_enabled") != 0,
                realtime_metadata_auto_match_enabled: row
                    .get::<i64, _>("realtime_metadata_auto_match_enabled")
                    != 0,
                incremental_schedule: row.get("incremental_schedule"),
                reconciliation_schedule: row.get("reconciliation_schedule"),
                metadata_schedule: row.get("metadata_schedule"),
                scan_concurrency: row.get("scan_concurrency"),
                probe_concurrency: row.get("probe_concurrency"),
                last_scan_at: row.get("last_scan_at"),
                scraper_id: row.get("scraper_id"),
                scrapers: scrapers.remove(&id).unwrap_or_default(),
                chapter_source_id: row.get("chapter_source_id"),
                cover_image_path: row.get("cover_image_path"),
                cover_image_content_type: row.get("cover_image_content_type"),
                cover_image_size: row.get("cover_image_size"),
                cover_image_tag: row.get("cover_image_tag"),
                media_strategy_json: row.get("media_strategy_json"),
            });
        }
        Ok(libraries)
    }

    /// Return only the identity data needed to map an external migration
    /// library.  The migration path must not load disabled libraries,
    /// scraper configuration, or other administrative settings.
    pub(crate) async fn list_enabled_library_identities(
        &self,
    ) -> Result<Vec<StoredLibraryIdentity>, StorageError> {
        let rows = self
            .query(
                "SELECT l.id, l.name, r.canonical_path
                 FROM libraries l
                 LEFT JOIN library_roots r ON r.library_id = l.id
                 WHERE l.is_enabled = 1
                 ORDER BY l.name, l.id, r.canonical_path, r.id",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut identities = Vec::<StoredLibraryIdentity>::new();
        for row in rows {
            let id: String = row.get("id");
            if let Some(identity) = identities.last_mut().filter(|identity| identity.id == id) {
                if let Some(root_path) = row.get::<Option<String>, _>("canonical_path") {
                    identity.root_paths.push(root_path);
                }
                continue;
            }
            identities.push(StoredLibraryIdentity {
                id,
                name: row.get("name"),
                root_paths: row
                    .get::<Option<String>, _>("canonical_path")
                    .into_iter()
                    .collect(),
            });
        }
        Ok(identities)
    }

    pub(crate) async fn find_library(
        &self,
        id: &str,
    ) -> Result<Option<StoredLibrary>, StorageError> {
        let row = self
            .query(
            "SELECT id, name, kind, is_enabled, realtime_watch_enabled,
                    realtime_metadata_auto_match_enabled,
                    incremental_schedule, reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, last_scan_at, scraper_id, chapter_source_id,
                    cover_image_path, cover_image_content_type, cover_image_size, cover_image_tag,
                    media_strategy_json
             FROM libraries WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(row) = row else {
            return Ok(None);
        };
        let stored_id: String = row.get("id");
        Ok(Some(StoredLibrary {
            id: stored_id.clone(),
            name: row.get("name"),
            kind: row.get("kind"),
            is_enabled: row.get::<i64, _>("is_enabled") != 0,
            realtime_watch_enabled: row.get::<i64, _>("realtime_watch_enabled") != 0,
            realtime_metadata_auto_match_enabled: row
                .get::<i64, _>("realtime_metadata_auto_match_enabled")
                != 0,
            incremental_schedule: row.get("incremental_schedule"),
            reconciliation_schedule: row.get("reconciliation_schedule"),
            metadata_schedule: row.get("metadata_schedule"),
            scan_concurrency: row.get("scan_concurrency"),
            probe_concurrency: row.get("probe_concurrency"),
            last_scan_at: row.get("last_scan_at"),
            scraper_id: row.get("scraper_id"),
            scrapers: self.list_library_scrapers(&stored_id).await?,
            chapter_source_id: row.get("chapter_source_id"),
            cover_image_path: row.get("cover_image_path"),
            cover_image_content_type: row.get("cover_image_content_type"),
            cover_image_size: row.get("cover_image_size"),
            cover_image_tag: row.get("cover_image_tag"),
            media_strategy_json: row.get("media_strategy_json"),
        }))
    }

    pub(crate) async fn register_auto_library_cover_task(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let assignment = self
            .ensure_library_task_plan(
                &mut transaction,
                "AUTO_LIBRARY_COVER",
                "自动生成媒体库封面",
                "首次达到至少 9 张海报后，随机选择 9 张海报生成带媒体库名称的旋转堆叠封面；管理员可手动执行或按计划重跑。",
                "SYSTEM",
                None,
                None,
                false,
                "{}",
            )
            .await?;
        self.query(
            "INSERT INTO scheduled_task_plan_libraries (plan_id, library_id)
             VALUES (?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(&assignment.id)
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let result = self
            .query(
                "INSERT INTO scheduled_task_configs (
                    owner_type, owner_id, task_type, task_name, task_description,
                    source_type, plugin_id, cron_or_interval, is_enabled,
                    resource_limit_json, plan_id
                 ) VALUES (
                    'LIBRARY', ?, 'AUTO_LIBRARY_COVER', ?, ?,
                    'SYSTEM', NULL, ?, ?, ?, ?
                 ) ON CONFLICT(owner_type, owner_id, task_type) DO UPDATE SET
                    task_name = excluded.task_name,
                    task_description = excluded.task_description,
                    source_type = excluded.source_type,
                    resource_limit_json = excluded.resource_limit_json,
                    plan_id = excluded.plan_id,
                    updated_at = unixepoch()",
            )
            .bind(library_id)
            .bind(&assignment.task_name)
            .bind(&assignment.task_description)
            .bind(assignment.schedule.as_deref())
            .bind(database_flag(assignment.is_enabled))
            .bind(&assignment.resource_limit_json)
            .bind(&assignment.id)
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
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn create_library_cover_job(
        &self,
        id: &str,
        library_id: &str,
        is_manual: bool,
    ) -> Result<bool, StorageError> {
        self.query(
            "INSERT INTO library_cover_jobs (id, library_id, is_manual, status)
             VALUES (?, ?, ?, 'PENDING')
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(library_id)
        .bind(database_flag(is_manual))
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_library_cover_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredLibraryCoverJob>, StorageError> {
        self.query(
            "SELECT id, library_id, is_manual, status, processed_count, total_count,
                    error, created_at, updated_at, started_at, finished_at
             FROM library_cover_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_library_cover_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_library_cover_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredLibraryCoverJob>, StorageError> {
        let limit = limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE);
        let query = if status.is_some() {
            self.query(
                "SELECT id, library_id, is_manual, status, processed_count, total_count,
                        error, created_at, updated_at, started_at, finished_at
                 FROM library_cover_jobs WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset.max(0))
        } else {
            self.query(
                "SELECT id, library_id, is_manual, status, processed_count, total_count,
                        error, created_at, updated_at, started_at, finished_at
                 FROM library_cover_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset.max(0))
        };
        query
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(stored_library_cover_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_active_library_cover_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM library_cover_jobs
             WHERE status IN ('PENDING', 'RUNNING') ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_library_cover_job(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM library_cover_jobs
                WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
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

    pub(crate) async fn claim_library_cover_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE library_cover_jobs
             SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'PENDING'",
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

    pub(crate) async fn update_library_cover_job_progress(
        &self,
        id: &str,
        processed_count: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE library_cover_jobs
             SET processed_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(processed_count)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_library_cover_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE library_cover_jobs
             SET status = ?, error = ?, processed_count = CASE
                    WHEN ? = 'COMPLETED' THEN total_count ELSE processed_count END,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(status)
        .bind(error)
        .bind(status)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_library(&self, id: &str) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "DELETE FROM scheduled_task_configs
             WHERE owner_type = 'LIBRARY' AND owner_id = ?",
        )
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "DELETE FROM strm_probe_jobs
             WHERE library_id = ?
                OR target_scan_job_id IN (
                    SELECT id FROM scan_jobs WHERE library_id = ?
                )",
        )
        .bind(id)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let deleted = self
            .query("DELETE FROM libraries WHERE id = ?")
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected()
            == 1;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(deleted)
    }

    pub(crate) async fn update_library_settings(
        &self,
        library_id: &str,
        settings: LibrarySettingsUpdate<'_>,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let exists: i64 = self
            .query_scalar(
                "SELECT CASE WHEN EXISTS(SELECT 1 FROM libraries WHERE id = ?) THEN 1 ELSE 0 END",
            )
            .bind(library_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if exists == 0 {
            return Ok(false);
        }

        if let Some(value) = settings.name {
            self.query(
                "UPDATE libraries
                 SET name = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.kind {
            self.query(
                "UPDATE libraries
                 SET kind = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.is_enabled {
            self.query(
                "UPDATE libraries
                 SET is_enabled = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(database_flag(value))
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.realtime_watch_enabled {
            self.query(
                "UPDATE libraries
                 SET realtime_watch_enabled = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(database_flag(value))
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.realtime_metadata_auto_match_enabled {
            self.query(
                "UPDATE libraries
                 SET realtime_metadata_auto_match_enabled = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(database_flag(value))
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.reconciliation_schedule {
            self.query(
                "UPDATE libraries
                 SET reconciliation_schedule = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.metadata_schedule {
            self.query(
                "UPDATE libraries
                 SET metadata_schedule = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(scrapers) = settings.scrapers {
            self.query("DELETE FROM library_scrapers WHERE library_id = ?")
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for scraper in scrapers {
                self.query(
                    "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
                     VALUES (?, ?, ?, ?)",
                )
                .bind(library_id)
                .bind(&scraper.scraper_id)
                .bind(scraper.position)
                .bind(scraper.role.as_str())
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
            let primary_scraper = scrapers.first().map(|scraper| scraper.scraper_id.as_str());
            self.query(
                "UPDATE libraries
                 SET scraper_id = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(primary_scraper)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        } else if let Some(value) = settings.scraper_id {
            self.query("DELETE FROM library_scrapers WHERE library_id = ?")
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if let Some(scraper_id) = value {
                self.query(
                    "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
                     VALUES (?, ?, 0, 'PRIMARY')",
                )
                .bind(library_id)
                .bind(scraper_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
            self.query(
                "UPDATE libraries
                 SET scraper_id = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.chapter_source_id {
            self.query(
                "UPDATE libraries
                 SET chapter_source_id = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.media_strategy_json {
            self.query(
                "UPDATE libraries
                 SET media_strategy_json = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.scan_concurrency {
            self.query(
                "UPDATE libraries
                 SET scan_concurrency = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.probe_concurrency {
            self.query(
                "UPDATE libraries
                 SET probe_concurrency = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }

        let current: (Option<String>, Option<String>, i64, i64, Option<String>) = self
            .query_as(
                "SELECT reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, scraper_id
             FROM libraries WHERE id = ?",
            )
            .bind(library_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let resources = format!(
            "{{\"scanConcurrency\":{},\"probeConcurrency\":{}}}",
            current.2, current.3
        );
        let task_configs = [
            (
                "RECONCILIATION_SCAN",
                current.0.as_deref(),
                resources.as_str(),
            ),
            ("METADATA_PARSE", current.1.as_deref(), "{}"),
        ];
        for (task_type, schedule, resource_limit_json) in task_configs {
            self.query(
                "UPDATE scheduled_task_configs
                 SET cron_or_interval = ?,
                     is_enabled = ?,
                     resource_limit_json = ?,
                     source_type = CASE
                         WHEN task_type = 'METADATA_PARSE' AND ? IS NOT NULL THEN 'PLUGIN'
                         ELSE 'SYSTEM'
                     END,
                     plugin_id = CASE
                         WHEN task_type = 'METADATA_PARSE' THEN ?
                         ELSE NULL
                     END,
                     updated_at = unixepoch()
                 WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = ?",
            )
            .bind(schedule)
            .bind(database_flag(schedule.is_some()))
            .bind(resource_limit_json)
            .bind(current.4.as_deref())
            .bind(current.4.as_deref())
            .bind(library_id)
            .bind(task_type)
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
        Ok(true)
    }

    pub(crate) async fn list_scheduled_task_configs(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<StoredScheduledTaskConfig>, i64), StorageError> {
        let total = self
            .query_scalar::<i64>("SELECT COUNT(*) FROM scheduled_task_configs")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let rows = self
            .query(
                "SELECT s.owner_type, s.owner_id, s.task_type, s.plan_id, s.task_name,
                    s.task_description, s.source_type, s.plugin_id,
                    s.cron_or_interval, s.is_enabled, s.resource_limit_json,
                    s.created_at, s.updated_at,
                    l.name AS library_name
             FROM scheduled_task_configs s
             LEFT JOIN libraries l
               ON s.owner_type = 'LIBRARY' AND l.id = s.owner_id
             ORDER BY s.updated_at DESC, s.owner_type, s.owner_id, s.task_type
             LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((rows.into_iter().map(stored_scheduled_task).collect(), total))
    }

    pub(crate) async fn list_scheduled_task_plans(
        &self,
        offset: i64,
        limit: i64,
        task_type: Option<&str>,
        search: Option<&str>,
    ) -> Result<(Vec<StoredScheduledTaskPlan>, i64), StorageError> {
        let search = search.map(str::trim).filter(|value| !value.is_empty());
        let search_pattern = search.map(|value| format!("%{}%", value.to_ascii_lowercase()));
        let filter = "WHERE (? IS NULL OR p.task_type = ?)
                       AND (? IS NULL OR lower(p.plan_name) LIKE ?
                            OR lower(p.task_name) LIKE ?
                            OR lower(p.task_type) LIKE ?)";
        let total = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(*) FROM scheduled_task_plans p {filter}"
            )))
            .bind(task_type)
            .bind(task_type)
            .bind(search_pattern.as_deref())
            .bind(search_pattern.as_deref())
            .bind(search_pattern.as_deref())
            .bind(search_pattern.as_deref())
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let rows = self
            .query(sqlx::AssertSqlSafe(format!(
                "SELECT p.id, p.task_type, p.plan_name, p.task_name,
                        p.task_description, p.source_type, p.plugin_id,
                        p.cron_or_interval, p.is_enabled, p.resource_limit_json,
                        p.scope_type, p.is_default, p.created_at, p.updated_at
                 FROM scheduled_task_plans p
                 {filter}
                 ORDER BY p.updated_at DESC, p.task_type, p.plan_name, p.id
                 LIMIT ? OFFSET ?"
            )))
            .bind(task_type)
            .bind(task_type)
            .bind(search_pattern.as_deref())
            .bind(search_pattern.as_deref())
            .bind(search_pattern.as_deref())
            .bind(search_pattern.as_deref())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut plans = rows
            .into_iter()
            .map(stored_scheduled_task_plan)
            .collect::<Vec<_>>();
        let plan_ids = plans.iter().map(|plan| plan.id.clone()).collect::<Vec<_>>();
        self.load_scheduled_task_plan_libraries(&mut plans, &plan_ids)
            .await?;
        Ok((plans, total))
    }

    async fn load_scheduled_task_plan_libraries(
        &self,
        plans: &mut [StoredScheduledTaskPlan],
        plan_ids: &[String],
    ) -> Result<(), StorageError> {
        if plan_ids.is_empty() {
            return Ok(());
        }
        let placeholders = std::iter::repeat_n("?", plan_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut query = self.query(sqlx::AssertSqlSafe(format!(
            "SELECT pl.plan_id, l.id AS library_id, l.name AS library_name
             FROM scheduled_task_plan_libraries pl
             JOIN libraries l ON l.id = pl.library_id
             WHERE pl.plan_id IN ({placeholders})
             ORDER BY l.name, l.id"
        )));
        for plan_id in plan_ids {
            query = query.bind(plan_id);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut by_plan = HashMap::<String, Vec<StoredScheduledTaskPlanLibrary>>::new();
        for row in rows {
            by_plan
                .entry(row.get("plan_id"))
                .or_default()
                .push(StoredScheduledTaskPlanLibrary {
                    id: row.get("library_id"),
                    name: row.get("library_name"),
                });
        }
        for plan in plans {
            plan.libraries = by_plan.remove(&plan.id).unwrap_or_default();
        }
        Ok(())
    }

    pub(crate) async fn find_scheduled_task_plan(
        &self,
        plan_id: &str,
    ) -> Result<Option<StoredScheduledTaskPlan>, StorageError> {
        let row = self
            .query(
                "SELECT id, task_type, plan_name, task_name, task_description,
                        source_type, plugin_id, cron_or_interval, is_enabled,
                        resource_limit_json, scope_type, is_default, created_at, updated_at
                 FROM scheduled_task_plans WHERE id = ?",
            )
            .bind(plan_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut plans = vec![stored_scheduled_task_plan(row)];
        self.load_scheduled_task_plan_libraries(&mut plans, &[plan_id.to_owned()])
            .await?;
        Ok(plans.pop())
    }

    async fn validate_scheduled_task_plan_libraries(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        task_type: &str,
        source_type: &str,
        plugin_id: Option<&str>,
        library_ids: &[String],
    ) -> Result<(), StorageError> {
        let mut seen = HashSet::with_capacity(library_ids.len());
        for library_id in library_ids {
            if !seen.insert(library_id) {
                return Err(StorageError::Conflict(
                    "同一媒体库不能在计划中重复选择".to_owned(),
                ));
            }
            let config: Option<(String, Option<String>)> = self
                .query_as(
                    "SELECT source_type, plugin_id
                     FROM scheduled_task_configs
                     WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = ?",
                )
                .bind(library_id)
                .bind(task_type)
                .fetch_optional(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let Some((actual_source_type, actual_plugin_id)) = config else {
                return Err(StorageError::Conflict(format!(
                    "媒体库 {library_id} 尚未注册任务 {task_type}"
                )));
            };
            if actual_source_type != source_type || actual_plugin_id.as_deref() != plugin_id {
                return Err(StorageError::Conflict(
                    "一个执行计划不能混合不同的插件来源".to_owned(),
                ));
            }
        }
        if library_ids.is_empty() {
            return Err(StorageError::Conflict(
                "执行计划至少需要一个媒体库".to_owned(),
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_scheduled_task_plan(
        &self,
        id: &str,
        task_type: &str,
        plan_name: &str,
        task_name: &str,
        task_description: &str,
        source_type: &str,
        plugin_id: Option<&str>,
        schedule: Option<&str>,
        is_enabled: bool,
        resource_limit_json: &str,
        library_ids: &[String],
    ) -> Result<Option<StoredScheduledTaskPlan>, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.validate_scheduled_task_plan_libraries(
            &mut transaction,
            task_type,
            source_type,
            plugin_id,
            library_ids,
        )
        .await?;
        self.query(
            "INSERT INTO scheduled_task_plans (
                id, task_type, plan_name, task_name, task_description,
                source_type, plugin_id, cron_or_interval, is_enabled,
                resource_limit_json, scope_type, is_default
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'LIBRARY', 0)",
        )
        .bind(id)
        .bind(task_type)
        .bind(plan_name)
        .bind(task_name)
        .bind(task_description)
        .bind(source_type)
        .bind(plugin_id)
        .bind(schedule)
        .bind(database_flag(is_enabled))
        .bind(resource_limit_json)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.move_libraries_to_scheduled_task_plan(
            &mut transaction,
            id,
            task_type,
            task_name,
            task_description,
            source_type,
            plugin_id,
            schedule,
            is_enabled,
            resource_limit_json,
            library_ids,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.find_scheduled_task_plan(id).await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn update_scheduled_task_plan(
        &self,
        plan_id: &str,
        plan_name: &str,
        schedule: Option<&str>,
        is_enabled: bool,
        library_ids: &[String],
        task_name: &str,
        task_description: &str,
        source_type: &str,
        plugin_id: Option<&str>,
        resource_limit_json: &str,
    ) -> Result<Option<StoredScheduledTaskPlan>, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let existing: Option<(String, String, i64)> = self
            .query_as(
                "SELECT task_type, scope_type, is_default
                 FROM scheduled_task_plans WHERE id = ?",
            )
            .bind(plan_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((task_type, scope_type, is_default)) = existing else {
            return Ok(None);
        };
        if scope_type != "LIBRARY" {
            return Err(StorageError::Conflict(
                "全局插件计划不能通过媒体库计划接口修改".to_owned(),
            ));
        }
        self.validate_scheduled_task_plan_libraries(
            &mut transaction,
            &task_type,
            source_type,
            plugin_id,
            library_ids,
        )
        .await?;
        let current_ids: Vec<String> = self
            .query_scalar("SELECT library_id FROM scheduled_task_plan_libraries WHERE plan_id = ?")
            .bind(plan_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if is_default != 0 && current_ids.iter().any(|id| !library_ids.contains(id)) {
            return Err(StorageError::Conflict(
                "默认计划不能移出媒体库，请创建新的自定义计划".to_owned(),
            ));
        }
        self.query(
            "UPDATE scheduled_task_plans
             SET plan_name = ?, cron_or_interval = ?, is_enabled = ?,
                 task_name = ?, task_description = ?, source_type = ?,
                 plugin_id = ?, resource_limit_json = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(plan_name)
        .bind(schedule)
        .bind(database_flag(is_enabled))
        .bind(task_name)
        .bind(task_description)
        .bind(source_type)
        .bind(plugin_id)
        .bind(resource_limit_json)
        .bind(plan_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if is_default == 0 {
            let removed_ids = current_ids
                .into_iter()
                .filter(|id| !library_ids.contains(id))
                .collect::<Vec<_>>();
            if !removed_ids.is_empty() {
                let default_plan_id: Option<String> = self
                    .query_scalar(
                        "SELECT id FROM scheduled_task_plans
                         WHERE task_type = ? AND scope_type = 'LIBRARY' AND is_default = 1
                           AND id <> ?
                           AND source_type = ?
                           AND (plugin_id = ? OR (plugin_id IS NULL AND ? IS NULL))
                         ORDER BY id LIMIT 1",
                    )
                    .bind(&task_type)
                    .bind(plan_id)
                    .bind(source_type)
                    .bind(plugin_id)
                    .bind(plugin_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                let Some(default_plan_id) = default_plan_id else {
                    return Err(StorageError::Conflict(
                        "没有可接收移出媒体库的默认计划".to_owned(),
                    ));
                };
                let placeholders = std::iter::repeat_n("?", removed_ids.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut update_query = self.query(sqlx::AssertSqlSafe(format!(
                    "UPDATE scheduled_task_configs SET plan_id = ?, updated_at = unixepoch()
                     WHERE owner_type = 'LIBRARY' AND task_type = ?
                       AND owner_id IN ({placeholders})"
                )));
                update_query = update_query.bind(&default_plan_id).bind(&task_type);
                for library_id in &removed_ids {
                    update_query = update_query.bind(library_id);
                }
                update_query
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                for library_id in removed_ids {
                    self.query(
                        "DELETE FROM scheduled_task_plan_libraries
                         WHERE plan_id = ? AND library_id = ?",
                    )
                    .bind(plan_id)
                    .bind(&library_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                    self.query(
                        "INSERT INTO scheduled_task_plan_libraries (plan_id, library_id)
                         VALUES (?, ?) ON CONFLICT DO NOTHING",
                    )
                    .bind(&default_plan_id)
                    .bind(library_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                }
            }
        }
        self.move_libraries_to_scheduled_task_plan(
            &mut transaction,
            plan_id,
            &task_type,
            task_name,
            task_description,
            source_type,
            plugin_id,
            schedule,
            is_enabled,
            resource_limit_json,
            library_ids,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.find_scheduled_task_plan(plan_id).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn move_libraries_to_scheduled_task_plan(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        plan_id: &str,
        task_type: &str,
        task_name: &str,
        task_description: &str,
        source_type: &str,
        plugin_id: Option<&str>,
        schedule: Option<&str>,
        is_enabled: bool,
        resource_limit_json: &str,
        library_ids: &[String],
    ) -> Result<(), StorageError> {
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut delete_query = self.query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM scheduled_task_plan_libraries
             WHERE library_id IN ({placeholders})
               AND plan_id IN (SELECT id FROM scheduled_task_plans WHERE task_type = ?)"
        )));
        for library_id in library_ids {
            delete_query = delete_query.bind(library_id);
        }
        delete_query
            .bind(task_type)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut update_query = self.query(sqlx::AssertSqlSafe(format!(
            "UPDATE scheduled_task_configs
             SET plan_id = ?, task_name = ?, task_description = ?, source_type = ?,
                 plugin_id = ?, cron_or_interval = ?, is_enabled = ?,
                 resource_limit_json = ?, updated_at = unixepoch()
             WHERE owner_type = 'LIBRARY' AND task_type = ?
               AND owner_id IN ({placeholders})"
        )));
        update_query = update_query
            .bind(plan_id)
            .bind(task_name)
            .bind(task_description)
            .bind(source_type)
            .bind(plugin_id)
            .bind(schedule)
            .bind(database_flag(is_enabled))
            .bind(resource_limit_json)
            .bind(task_type);
        for library_id in library_ids {
            update_query = update_query.bind(library_id);
        }
        update_query
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for library_id in library_ids {
            self.query(
                "INSERT INTO scheduled_task_plan_libraries (plan_id, library_id)
                 VALUES (?, ?)",
            )
            .bind(plan_id)
            .bind(library_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    pub(crate) async fn upsert_scheduled_task_config(
        &self,
        owner_type: &str,
        owner_id: &str,
        task_type: &str,
        schedule: Option<&str>,
        is_enabled: bool,
    ) -> Result<Option<StoredScheduledTaskConfig>, StorageError> {
        let result = self
            .query(
                "UPDATE scheduled_task_configs
             SET cron_or_interval = ?, is_enabled = ?, updated_at = unixepoch()
             WHERE owner_type = ? AND owner_id = ? AND task_type = ?",
            )
            .bind(schedule)
            .bind(database_flag(is_enabled))
            .bind(owner_type)
            .bind(owner_id)
            .bind(task_type)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.find_scheduled_task_config(owner_type, owner_id, task_type)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn register_plugin_scheduled_task(
        &self,
        owner_type: &str,
        owner_id: &str,
        task_type: &str,
        task_name: &str,
        task_description: &str,
        plugin_id: &str,
        schedule: &str,
        is_enabled: bool,
        resource_limit_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO scheduled_task_configs (
                owner_type, owner_id, task_type, task_name, task_description,
                source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
             ) VALUES (?, ?, ?, ?, ?, 'PLUGIN', ?, ?, ?, ?)
             ON CONFLICT(owner_type, owner_id, task_type) DO UPDATE SET
                task_name = excluded.task_name,
                task_description = excluded.task_description,
                source_type = excluded.source_type,
                plugin_id = excluded.plugin_id,
                cron_or_interval = excluded.cron_or_interval,
                is_enabled = excluded.is_enabled,
                resource_limit_json = excluded.resource_limit_json,
                updated_at = unixepoch()",
        )
        .bind(owner_type)
        .bind(owner_id)
        .bind(task_type)
        .bind(task_name)
        .bind(task_description)
        .bind(plugin_id)
        .bind(schedule)
        .bind(database_flag(is_enabled))
        .bind(resource_limit_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn disable_plugin_scheduled_task(
        &self,
        plugin_id: &str,
        task_type: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scheduled_task_configs
             SET is_enabled = 0, updated_at = unixepoch()
             WHERE source_type = 'PLUGIN' AND plugin_id = ? AND task_type = ?",
        )
        .bind(plugin_id)
        .bind(task_type)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn upsert_strm_media_info_task(
        &self,
        schedule: &str,
        is_enabled: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO scheduled_task_configs (
                owner_type, owner_id, task_type, task_name, task_description,
                source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
             ) VALUES (
                'GLOBAL', 'global', 'STRM_MEDIA_INFO', 'STRM 媒体信息扫描',
                '按插件配置周期扫描选定媒体库的 STRM 外部媒体信息并写入 JSON 旁车。',
                'PLUGIN', 'org.lux.strm-media-info', ?, ?, '{}'
             )
             ON CONFLICT(owner_type, owner_id, task_type) DO UPDATE SET
                task_name = excluded.task_name,
                task_description = excluded.task_description,
                source_type = excluded.source_type,
                plugin_id = excluded.plugin_id,
                cron_or_interval = excluded.cron_or_interval,
                is_enabled = excluded.is_enabled,
                updated_at = unixepoch()",
        )
        .bind(schedule)
        .bind(database_flag(is_enabled))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn disable_strm_media_info_task(&self) -> Result<(), StorageError> {
        self.query(
            "UPDATE scheduled_task_configs
             SET is_enabled = 0, cron_or_interval = NULL, updated_at = unixepoch()
             WHERE owner_type = 'GLOBAL' AND owner_id = 'global'
               AND task_type = 'STRM_MEDIA_INFO'",
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn disable_chapter_detection_tasks(&self) -> Result<(), StorageError> {
        self.query(
            "UPDATE scheduled_task_configs
             SET is_enabled = 0, cron_or_interval = NULL, updated_at = unixepoch()
             WHERE task_type = 'CHAPTER_DETECTION'",
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn upsert_chapter_detection_task(
        &self,
        library_id: &str,
        plugin_id: &str,
        schedule: &str,
        is_enabled: bool,
        concurrency: i64,
        intro_window_seconds: i64,
        credits_window_seconds: i64,
        match_threshold: u32,
    ) -> Result<(), StorageError> {
        let resource_limit_json = format!(
            "{{\"concurrency\":{concurrency},\"introWindowSeconds\":{intro_window_seconds},\"creditsWindowSeconds\":{credits_window_seconds},\"matchThreshold\":{match_threshold}}}"
        );
        self.query(
            "INSERT INTO scheduled_task_configs (
                owner_type, owner_id, task_type, task_name, task_description,
                source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
             ) VALUES (
                'LIBRARY', ?, 'CHAPTER_DETECTION', '片头片尾检测',
                '按插件配置比较同季度分集并保存片头片尾特殊章节。',
                'PLUGIN', ?, ?, ?, ?
             )
             ON CONFLICT(owner_type, owner_id, task_type) DO UPDATE SET
                task_name = excluded.task_name,
                task_description = excluded.task_description,
                source_type = excluded.source_type,
                plugin_id = excluded.plugin_id,
                cron_or_interval = excluded.cron_or_interval,
                is_enabled = excluded.is_enabled,
                resource_limit_json = excluded.resource_limit_json,
                updated_at = unixepoch()",
        )
        .bind(library_id)
        .bind(plugin_id)
        .bind(schedule)
        .bind(database_flag(is_enabled))
        .bind(resource_limit_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_scheduled_task_config(
        &self,
        owner_type: &str,
        owner_id: &str,
        task_type: &str,
    ) -> Result<Option<StoredScheduledTaskConfig>, StorageError> {
        self.query(
            "SELECT s.owner_type, s.owner_id, s.task_type, s.plan_id, s.task_name,
                    s.task_description, s.source_type, s.plugin_id,
                    s.cron_or_interval, s.is_enabled, s.resource_limit_json,
                    s.created_at, s.updated_at,
                    l.name AS library_name
             FROM scheduled_task_configs s
             LEFT JOIN libraries l
               ON s.owner_type = 'LIBRARY' AND l.id = s.owner_id
             WHERE s.owner_type = ? AND s.owner_id = ? AND s.task_type = ?",
        )
        .bind(owner_type)
        .bind(owner_id)
        .bind(task_type)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_scheduled_task))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn library_exists(&self, library_id: &str) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(SELECT 1 FROM libraries WHERE id = ?) THEN 1 ELSE 0 END",
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

    pub(crate) async fn update_library_cover(
        &self,
        library_id: &str,
        path: &str,
        content_type: &str,
        size: i64,
        tag: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE libraries
             SET cover_image_path = ?,
                 cover_image_content_type = ?,
                 cover_image_size = ?,
                 cover_image_tag = ?,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(path)
        .bind(content_type)
        .bind(size)
        .bind(tag)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_library_cover_if_missing(
        &self,
        library_id: &str,
        path: &str,
        content_type: &str,
        size: i64,
        tag: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE libraries
             SET cover_image_path = ?,
                 cover_image_content_type = ?,
                 cover_image_size = ?,
                 cover_image_tag = ?,
                 updated_at = unixepoch()
             WHERE id = ? AND cover_image_path IS NULL",
        )
        .bind(path)
        .bind(content_type)
        .bind(size)
        .bind(tag)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_library_cover_if_auto(
        &self,
        library_id: &str,
        path: &str,
        content_type: &str,
        size: i64,
        tag: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE libraries
             SET cover_image_path = ?,
                 cover_image_content_type = ?,
                 cover_image_size = ?,
                 cover_image_tag = ?,
                 updated_at = unixepoch()
             WHERE id = ? AND (cover_image_path IS NULL OR cover_image_path = ?)",
        )
        .bind(path)
        .bind(content_type)
        .bind(size)
        .bind(tag)
        .bind(library_id)
        .bind(path)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}
