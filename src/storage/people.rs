use super::*;

impl Database {
    pub(crate) async fn sync_person_index_rebuild_jobs(
        &self,
        schema_version: i64,
    ) -> Result<Vec<StoredPersonIndexRebuildJob>, StorageError> {
        for library_id in self.list_enabled_library_ids().await? {
            self.query(
                "INSERT INTO person_index_rebuild_jobs (library_id, schema_version)
                 VALUES (?, ?)
                 ON CONFLICT(library_id) DO UPDATE SET
                    schema_version = excluded.schema_version,
                    status = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN 'QUEUED'
                        WHEN person_index_rebuild_jobs.status = 'RUNNING'
                            AND person_index_rebuild_jobs.updated_at < unixepoch() - 60
                            THEN 'QUEUED'
                        ELSE person_index_rebuild_jobs.status
                    END,
                    cursor_id = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN NULL
                        WHEN person_index_rebuild_jobs.status = 'RUNNING'
                            AND person_index_rebuild_jobs.updated_at < unixepoch() - 60
                            THEN person_index_rebuild_jobs.cursor_id
                        ELSE person_index_rebuild_jobs.cursor_id
                    END,
                    processed_count = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN 0
                        ELSE person_index_rebuild_jobs.processed_count
                    END,
                    total_count = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN 0
                        ELSE person_index_rebuild_jobs.total_count
                    END,
                    cancel_requested = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN 0
                        WHEN person_index_rebuild_jobs.status = 'RUNNING'
                            AND person_index_rebuild_jobs.updated_at < unixepoch() - 60
                            THEN 0
                        ELSE person_index_rebuild_jobs.cancel_requested
                    END,
                    run_token = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN NULL
                        WHEN person_index_rebuild_jobs.status = 'RUNNING'
                            AND person_index_rebuild_jobs.updated_at < unixepoch() - 60
                            THEN NULL
                        ELSE person_index_rebuild_jobs.run_token
                    END,
                    error = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN NULL
                        ELSE person_index_rebuild_jobs.error
                    END,
                    updated_at = unixepoch()",
            )
            .bind(&library_id)
            .bind(schema_version)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.list_person_index_rebuild_jobs(0, 500).await
    }

    pub(crate) async fn list_person_index_rebuild_jobs(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredPersonIndexRebuildJob>, StorageError> {
        self.query(
            "SELECT library_id, status, cursor_id, processed_count, total_count,
                    cancel_requested
             FROM person_index_rebuild_jobs
             ORDER BY library_id
             LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_person_index_rebuild_job)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn get_person_index_rebuild_job(
        &self,
        library_id: &str,
    ) -> Result<Option<StoredPersonIndexRebuildJob>, StorageError> {
        self.query(
            "SELECT library_id, status, cursor_id, processed_count, total_count,
                    cancel_requested
             FROM person_index_rebuild_jobs
             WHERE library_id = ?",
        )
        .bind(library_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_person_index_rebuild_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_person_index_rebuild_jobs(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM person_index_rebuild_jobs")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_person_index_rebuild_job(
        &self,
        library_id: &str,
        schema_version: i64,
    ) -> Result<bool, StorageError> {
        let enabled = self
            .query_scalar::<i64>("SELECT 1 FROM libraries WHERE id = ? AND is_enabled = 1 LIMIT 1")
            .bind(library_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .is_some();
        if !enabled {
            return Ok(false);
        }
        self.query(
            "INSERT INTO person_index_rebuild_jobs (
                library_id, status, cursor_id, processed_count, total_count,
                cancel_requested, schema_version, run_token, error,
                created_at, updated_at, started_at, finished_at
             ) VALUES (?, 'QUEUED', NULL, 0, 0, 0, ?, NULL, NULL,
                       unixepoch(), unixepoch(), NULL, NULL)
             ON CONFLICT(library_id) DO UPDATE SET
                status = 'QUEUED', cursor_id = NULL, processed_count = 0,
                total_count = 0, cancel_requested = 0, schema_version = excluded.schema_version,
                run_token = NULL, error = NULL, updated_at = unixepoch(),
                started_at = NULL, finished_at = NULL",
        )
        .bind(library_id)
        .bind(schema_version)
        .execute(&self.pool)
        .await
        .map(|_| true)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_person_index_rebuild_job(
        &self,
        library_id: &str,
        run_token: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE person_index_rebuild_jobs
                 SET status = 'RUNNING', run_token = ?, started_at = unixepoch(),
                     updated_at = unixepoch()
                 WHERE library_id = ? AND status = 'QUEUED' AND cancel_requested = 0",
            )
            .bind(run_token)
            .bind(library_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn request_person_index_rebuild_job_cancel(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE person_index_rebuild_jobs
                 SET status = CASE WHEN status = 'QUEUED' THEN 'CANCELLED' ELSE status END,
                     cancel_requested = CASE WHEN status = 'QUEUED' THEN 0 ELSE 1 END,
                     run_token = CASE WHEN status = 'QUEUED' THEN NULL ELSE run_token END,
                     finished_at = CASE WHEN status = 'QUEUED' THEN unixepoch() ELSE finished_at END,
                     updated_at = unixepoch()
                 WHERE library_id = ? AND status IN ('QUEUED', 'RUNNING')",
            )
            .bind(library_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn person_index_rebuild_job_cancel_requested(
        &self,
        library_id: &str,
        run_token: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE
                        WHEN status = 'RUNNING' AND run_token = ? AND cancel_requested = 0
                            THEN 0
                        ELSE 1
                    END
             FROM person_index_rebuild_jobs
             WHERE library_id = ?",
        )
        .bind(run_token)
        .bind(library_id)
        .fetch_optional(&self.pool)
        .await
        .map(|value: Option<i64>| value != Some(0))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_person_index_rebuild_progress(
        &self,
        library_id: &str,
        run_token: &str,
        cursor_id: &str,
        processed_count: i64,
        total_count: i64,
    ) -> Result<Option<()>, StorageError> {
        let result = self
            .query(
                "UPDATE person_index_rebuild_jobs
                 SET cursor_id = ?, processed_count = ?, total_count = ?,
                     updated_at = unixepoch()
                 WHERE library_id = ? AND status = 'RUNNING' AND run_token = ?",
            )
            .bind(cursor_id)
            .bind(processed_count)
            .bind(total_count)
            .bind(library_id)
            .bind(run_token)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((result.rows_affected() == 1).then_some(()))
    }

    pub(crate) async fn finish_person_index_rebuild_job(
        &self,
        library_id: &str,
        run_token: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<bool, StorageError> {
        if !matches!(status, "COMPLETED" | "CANCELLED" | "FAILED") {
            return Err(StorageError::Conflict(
                "invalid person index rebuild status".to_owned(),
            ));
        }
        let result = self
            .query(
                "UPDATE person_index_rebuild_jobs
                 SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                     error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
                     run_token = NULL, finished_at = unixepoch(), updated_at = unixepoch()
                 WHERE library_id = ? AND status = 'RUNNING' AND run_token = ?",
            )
            .bind(status)
            .bind(error)
            .bind(library_id)
            .bind(run_token)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    #[cfg(test)]
    pub(crate) async fn replace_person_credits(
        &self,
        item_id: &str,
        credits: &[NewPersonCredit],
    ) -> Result<(), StorageError> {
        self.replace_person_credits_with_fingerprint(item_id, credits, None)
            .await
    }

    pub(crate) async fn replace_person_credits_with_fingerprint(
        &self,
        item_id: &str,
        credits: &[NewPersonCredit],
        source_fingerprint: Option<&str>,
    ) -> Result<(), StorageError> {
        let _metadata_write_guard = self.acquire_metadata_write_lock().await;
        let _write_guard = self.person_credits_write_lock.lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query("DELETE FROM person_credits WHERE item_id = ?")
            .bind(item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut seen_keys = HashSet::with_capacity(credits.len());
        let mut duplicates_skipped = 0;
        for credit in credits {
            let key = (
                credit.person_id.as_str(),
                credit.person_type.as_str(),
                credit.provider.as_str(),
                credit.role.as_str(),
            );
            if !seen_keys.insert(key) {
                duplicates_skipped += 1;
                continue;
            }
            let provider_ids_json = serde_json::to_string(&credit.provider_ids)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            let genres_json = serde_json::to_string(&credit.genres)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            let tags_json = serde_json::to_string(&credit.tags)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            let production_locations_json = serde_json::to_string(&credit.production_locations)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            let taglines_json = serde_json::to_string(&credit.taglines)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            self.query(
                "INSERT INTO person_credits (
                    item_id, person_id, person_type, person_name, provider, role,
                    sort_order, biography, birthday, deathday, known_for_department,
                    place_of_birth, provider_ids_json, genres_json, tags_json,
                    production_locations_json, premiere_date, production_year, taglines_json
                    , lux_person_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (item_id, person_type, provider, person_id, role) DO UPDATE SET
                    person_name = excluded.person_name,
                    sort_order = excluded.sort_order,
                    biography = excluded.biography,
                    birthday = excluded.birthday,
                    deathday = excluded.deathday,
                    known_for_department = excluded.known_for_department,
                    place_of_birth = excluded.place_of_birth,
                    provider_ids_json = excluded.provider_ids_json,
                    genres_json = excluded.genres_json,
                    tags_json = excluded.tags_json,
                    production_locations_json = excluded.production_locations_json,
                    premiere_date = excluded.premiere_date,
                    production_year = excluded.production_year,
                    taglines_json = excluded.taglines_json,
                    lux_person_id = excluded.lux_person_id",
            )
            .bind(item_id)
            .bind(&credit.person_id)
            .bind(&credit.person_type)
            .bind(&credit.person_name)
            .bind(&credit.provider)
            .bind(&credit.role)
            .bind(credit.sort_order)
            .bind(&credit.biography)
            .bind(&credit.birthday)
            .bind(&credit.deathday)
            .bind(&credit.known_for_department)
            .bind(&credit.place_of_birth)
            .bind(provider_ids_json)
            .bind(genres_json)
            .bind(tags_json)
            .bind(production_locations_json)
            .bind(&credit.premiere_date)
            .bind(credit.production_year)
            .bind(taglines_json)
            .bind(&credit.lux_person_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if duplicates_skipped > 0 {
            tracing::debug!(
                item_id,
                duplicates_skipped,
                "deduplicated person credits before indexing"
            );
        }
        self.query(
            "INSERT INTO person_index_item_state (
                item_id, source_fingerprint, relation_schema_version, updated_at
             ) VALUES (?, ?, 2, unixepoch())
             ON CONFLICT(item_id) DO UPDATE SET
                source_fingerprint = excluded.source_fingerprint,
                relation_schema_version = excluded.relation_schema_version,
                updated_at = excluded.updated_at",
        )
        .bind(item_id)
        .bind(source_fingerprint)
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
        Ok(())
    }

    pub(crate) async fn clear_person_credits(&self, item_id: &str) -> Result<u64, StorageError> {
        let _metadata_write_guard = self.acquire_metadata_write_lock().await;
        let _write_guard = self.person_credits_write_lock.lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query("DELETE FROM person_credits WHERE item_id = ?")
            .bind(item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM person_index_item_state WHERE item_id = ?")
            .bind(item_id)
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

    pub(crate) async fn resolve_or_create_canonical_person(
        &self,
        display_name: &str,
        provider: &str,
        provider_id: &str,
        match_method: &str,
        confidence: Option<f64>,
        evidence_json: &str,
    ) -> Result<StoredCanonicalPerson, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        if let Some(person) = self
            .query(
                "SELECT p.id
                 FROM people p
                 JOIN person_identities pi ON pi.person_id = p.id
                 WHERE pi.provider = ? AND pi.provider_id = ?",
            )
            .bind(provider)
            .bind(provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        {
            let stored = stored_canonical_person(person);
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(stored);
        }

        let person_id = loop {
            let sequence: i64 = self
                .query_scalar("INSERT INTO person_id_sequence DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let candidate = format!("lux-{sequence:06}");
            let exists: Option<i64> = self
                .query_scalar("SELECT 1 FROM people WHERE id = ?")
                .bind(&candidate)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if exists.is_none() {
                break candidate;
            }
        };
        self.query(
            "INSERT INTO people (
                id, display_name, directory_name, normalized_name, status, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 'ACTIVE', ?, ?)",
        )
        .bind(&person_id)
        .bind(display_name)
        .bind(display_name)
        .bind(normalize_person_name(display_name))
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "INSERT INTO person_identities (
                person_id, provider, provider_id, match_method, confidence,
                evidence_json, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(provider, provider_id) DO NOTHING",
        )
        .bind(&person_id)
        .bind(provider)
        .bind(provider_id)
        .bind(match_method)
        .bind(confidence)
        .bind(evidence_json)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        let person = self
            .query(
                "SELECT p.id
                 FROM people p
                 JOIN person_identities pi ON pi.person_id = p.id
                 WHERE pi.provider = ? AND pi.provider_id = ?",
            )
            .bind(provider)
            .bind(provider_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let stored = stored_canonical_person(person);
        if stored.id != person_id {
            self.query("DELETE FROM people WHERE id = ?")
                .bind(&person_id)
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
        Ok(stored)
    }

    pub(crate) async fn find_canonical_person_by_identity(
        &self,
        provider: &str,
        provider_id: &str,
    ) -> Result<Option<StoredCanonicalPerson>, StorageError> {
        self.query(
            "SELECT p.id
             FROM people p
             JOIN person_identities pi ON pi.person_id = p.id
             WHERE pi.provider = ? AND pi.provider_id = ?",
        )
        .bind(provider)
        .bind(provider_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_canonical_person))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_canonical_people_by_identities(
        &self,
        identities: &[(String, String)],
    ) -> Result<Vec<(String, String, String)>, StorageError> {
        const MAX_IDENTITIES_PER_QUERY: usize = 400;
        let mut matches = Vec::new();
        for chunk in identities.chunks(MAX_IDENTITIES_PER_QUERY) {
            if chunk.is_empty() {
                continue;
            }
            let conditions =
                std::iter::repeat_n("(pi.provider = ? AND pi.provider_id = ?)", chunk.len())
                    .collect::<Vec<_>>()
                    .join(" OR ");
            let query = format!(
                "SELECT pi.provider, pi.provider_id, p.id
                 FROM person_identities pi
                 JOIN people p ON p.id = pi.person_id
                 WHERE {conditions}
                 ORDER BY pi.provider, pi.provider_id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (provider, provider_id) in chunk {
                statement = statement.bind(provider).bind(provider_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            matches.extend(rows.into_iter().map(|row| {
                (
                    row.get::<String, _>("provider"),
                    row.get::<String, _>("provider_id"),
                    row.get::<String, _>("id"),
                )
            }));
        }
        Ok(matches)
    }

    pub(crate) async fn find_canonical_person_display_name(
        &self,
        person_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar::<String>("SELECT display_name FROM people WHERE id = ?")
            .bind(person_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_canonical_people_by_normalized_name(
        &self,
        normalized_name: &str,
    ) -> Result<Vec<StoredCanonicalPersonMatch>, StorageError> {
        let rows = self
            .query(
                "SELECT p.id, p.display_name, pc.birthday
                 FROM people p
                 LEFT JOIN person_credits pc
                   ON pc.lux_person_id = p.id AND pc.person_type = 'Actor'
                 WHERE p.status = 'ACTIVE' AND p.normalized_name = ?
                 ORDER BY p.id, pc.birthday",
            )
            .bind(normalized_name)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut matches = Vec::<StoredCanonicalPersonMatch>::new();
        for row in rows {
            let id: String = row.get("id");
            let display_name: String = row.get("display_name");
            if normalize_person_name(&display_name) != normalized_name {
                continue;
            }
            let birthday: Option<String> = row.try_get("birthday").ok();
            if let Some(existing) = matches.iter_mut().find(|candidate| candidate.id == id) {
                if let Some(birthday) = birthday.filter(|value| !value.trim().is_empty())
                    && !existing.birthdays.iter().any(|value| value == &birthday)
                {
                    existing.birthdays.push(birthday);
                }
            } else {
                matches.push(StoredCanonicalPersonMatch {
                    id,
                    birthdays: birthday
                        .filter(|value| !value.trim().is_empty())
                        .into_iter()
                        .collect(),
                });
            }
        }
        Ok(matches)
    }

    pub(crate) async fn enqueue_person_match_candidate(
        &self,
        item_id: &str,
        provider: &str,
        provider_id: &str,
        candidate_person_ids_json: &str,
        score: Option<f64>,
        evidence_json: &str,
    ) -> Result<String, StorageError> {
        let now = current_unix_timestamp();
        let candidate_id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO person_match_candidates (
                id, item_id, provider, provider_id, candidate_person_ids_json,
                status, score, evidence_json, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 'PENDING', ?, ?, ?, ?)
             ON CONFLICT(item_id, provider, provider_id) DO UPDATE SET
                candidate_person_ids_json = excluded.candidate_person_ids_json,
                status = CASE
                    WHEN person_match_candidates.status IN ('CONFIRMED', 'REJECTED')
                        THEN person_match_candidates.status
                    ELSE excluded.status
                END,
                score = excluded.score,
                evidence_json = excluded.evidence_json,
                updated_at = excluded.updated_at",
        )
        .bind(candidate_id)
        .bind(item_id)
        .bind(provider)
        .bind(provider_id)
        .bind(candidate_person_ids_json)
        .bind(score)
        .bind(evidence_json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query_scalar::<String>(
            "SELECT id FROM person_match_candidates
             WHERE item_id = ? AND provider = ? AND provider_id = ?",
        )
        .bind(item_id)
        .bind(provider)
        .bind(provider_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn restore_person_match_candidate(
        &self,
        restore: &PersonMatchCandidateRestore<'_>,
    ) -> Result<String, StorageError> {
        if !matches!(restore.status, "PENDING" | "CONFIRMED" | "REJECTED") {
            return Err(StorageError::Serialization(
                "invalid person match candidate status".to_owned(),
            ));
        }
        self.query(
            "INSERT INTO person_match_candidates (
                id, item_id, provider, provider_id, candidate_person_ids_json,
                status, score, evidence_json, target_person_id, previous_person_id,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(item_id, provider, provider_id) DO UPDATE SET
                candidate_person_ids_json = excluded.candidate_person_ids_json,
                status = CASE
                    WHEN person_match_candidates.status IN ('CONFIRMED', 'REJECTED')
                        AND excluded.status = 'PENDING'
                        THEN person_match_candidates.status
                    ELSE excluded.status
                END,
                score = excluded.score,
                evidence_json = excluded.evidence_json,
                target_person_id = COALESCE(excluded.target_person_id, person_match_candidates.target_person_id),
                previous_person_id = COALESCE(excluded.previous_person_id, person_match_candidates.previous_person_id),
                updated_at = excluded.updated_at",
        )
        .bind(restore.candidate_id)
        .bind(restore.item_id)
        .bind(restore.provider)
        .bind(restore.provider_id)
        .bind(restore.candidate_person_ids_json)
        .bind(restore.status)
        .bind(restore.score)
        .bind(restore.evidence_json)
        .bind(restore.target_person_id)
        .bind(restore.previous_person_id)
        .bind(restore.created_at)
        .bind(restore.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query_scalar::<String>(
            "SELECT id FROM person_match_candidates
             WHERE item_id = ? AND provider = ? AND provider_id = ?",
        )
        .bind(restore.item_id)
        .bind(restore.provider)
        .bind(restore.provider_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_pending_person_match_candidates(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM person_match_candidates WHERE status = 'PENDING'")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_pending_person_match_candidates(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredPersonMatchCandidate>, StorageError> {
        self.query(
            "SELECT id, item_id, provider, provider_id,
                    candidate_person_ids_json, status, score,
                    evidence_json, target_person_id, previous_person_id,
                    created_at, updated_at
             FROM person_match_candidates
             WHERE status = 'PENDING'
             ORDER BY updated_at DESC, id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_person_match_candidate)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_person_match_candidate(
        &self,
        candidate_id: &str,
    ) -> Result<Option<StoredPersonMatchCandidate>, StorageError> {
        self.query(
            "SELECT id, item_id, provider, provider_id,
                    candidate_person_ids_json, status, score,
                    evidence_json, target_person_id, previous_person_id,
                    created_at, updated_at
             FROM person_match_candidates WHERE id = ?",
        )
        .bind(candidate_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_person_match_candidate))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn reject_person_match_candidate(
        &self,
        candidate_id: &str,
        evidence_json: &str,
    ) -> Result<(), StorageError> {
        let result = self
            .query(
                "UPDATE person_match_candidates
                 SET status = 'REJECTED', evidence_json = ?, updated_at = ?
                 WHERE id = ? AND status = 'PENDING'",
            )
            .bind(evidence_json)
            .bind(current_unix_timestamp())
            .bind(candidate_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 0 {
            return Err(StorageError::Conflict(format!(
                "person match candidate '{candidate_id}' is missing or not pending"
            )));
        }
        Ok(())
    }

    pub(crate) async fn confirm_person_match_candidate(
        &self,
        candidate_id: &str,
        target_person_id: &str,
        evidence_json: &str,
    ) -> Result<StoredPersonIdentityMove, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let candidate = self
            .query(
                "SELECT id, item_id, provider, provider_id,
                        candidate_person_ids_json, status, score,
                        evidence_json, target_person_id, previous_person_id,
                        created_at, updated_at
                 FROM person_match_candidates WHERE id = ?",
            )
            .bind(candidate_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .map(stored_person_match_candidate)
            .ok_or_else(|| {
                StorageError::Conflict(format!("person match candidate '{candidate_id}' not found"))
            })?;
        if candidate.status != "PENDING" {
            return Err(StorageError::Conflict(format!(
                "person match candidate '{candidate_id}' is {}",
                candidate.status
            )));
        }
        let candidate_person_ids =
            serde_json::from_str::<Vec<String>>(&candidate.candidate_person_ids_json)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
        if !candidate_person_ids
            .iter()
            .any(|person_id| person_id == target_person_id)
        {
            return Err(StorageError::Conflict(
                "selected person is not one of the candidate matches".to_owned(),
            ));
        }
        let target_exists = self
            .query_scalar::<String>("SELECT id FROM people WHERE id = ?")
            .bind(target_person_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .is_some();
        if !target_exists {
            return Err(StorageError::Conflict(format!(
                "canonical person '{target_person_id}' does not exist"
            )));
        }
        let previous_person_id = self
            .query_scalar::<String>(
                "SELECT person_id FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if previous_person_id.as_deref() != Some(target_person_id) {
            self.query(
                "DELETE FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "INSERT INTO person_identities (
                    person_id, provider, provider_id, match_method, confidence,
                    evidence_json, created_at, updated_at
                 ) VALUES (?, ?, ?, 'MANUAL_CONFIRM', ?, ?, ?, ?)",
            )
            .bind(target_person_id)
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .bind(Some(1.0_f64))
            .bind(evidence_json)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "UPDATE person_credits SET lux_person_id = ?
                 WHERE provider = ? AND person_id = ?",
            )
            .bind(target_person_id)
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "UPDATE person_match_candidates
             SET status = 'CONFIRMED', evidence_json = ?,
                 target_person_id = ?, previous_person_id = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(evidence_json)
        .bind(target_person_id)
        .bind(previous_person_id.as_deref())
        .bind(now)
        .bind(candidate_id)
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
        Ok(StoredPersonIdentityMove { previous_person_id })
    }

    pub(crate) async fn undo_person_match_candidate(
        &self,
        candidate_id: &str,
        evidence_json: &str,
    ) -> Result<StoredPersonIdentityMove, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let candidate = self
            .query(
                "SELECT id, item_id, provider, provider_id,
                        candidate_person_ids_json, status, score,
                        evidence_json, target_person_id, previous_person_id,
                        created_at, updated_at
                 FROM person_match_candidates WHERE id = ?",
            )
            .bind(candidate_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .map(stored_person_match_candidate)
            .ok_or_else(|| {
                StorageError::Conflict(format!("person match candidate '{candidate_id}' not found"))
            })?;
        if candidate.status != "CONFIRMED" {
            return Err(StorageError::Conflict(format!(
                "person match candidate '{candidate_id}' is {}",
                candidate.status
            )));
        }
        let target_person_id = candidate.target_person_id.ok_or_else(|| {
            StorageError::Conflict(
                "confirmed person match has no recorded target identity".to_owned(),
            )
        })?;
        let current_owner = self
            .query_scalar::<String>(
                "SELECT person_id FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if current_owner.as_deref() != Some(target_person_id.as_str()) {
            return Err(StorageError::Conflict(
                "provider identity no longer belongs to the confirmed target".to_owned(),
            ));
        }
        let previous_person_id = candidate.previous_person_id;
        if let Some(previous_person_id) = previous_person_id.as_deref() {
            let previous_exists = self
                .query_scalar::<String>("SELECT id FROM people WHERE id = ?")
                .bind(previous_person_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .is_some();
            if !previous_exists {
                return Err(StorageError::Conflict(
                    "previous canonical person no longer exists".to_owned(),
                ));
            }
        }
        self.query(
            "DELETE FROM person_identities
             WHERE provider = ? AND provider_id = ?",
        )
        .bind(&candidate.provider)
        .bind(&candidate.provider_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if let Some(previous_person_id) = previous_person_id.as_deref() {
            self.query(
                "INSERT INTO person_identities (
                    person_id, provider, provider_id, match_method, confidence,
                    evidence_json, created_at, updated_at
                 ) VALUES (?, ?, ?, 'MANUAL_UNDO', ?, ?, ?, ?)",
            )
            .bind(previous_person_id)
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .bind(Some(1.0_f64))
            .bind(evidence_json)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "UPDATE person_credits SET lux_person_id = ?
             WHERE provider = ? AND person_id = ?",
        )
        .bind(previous_person_id.as_deref())
        .bind(&candidate.provider)
        .bind(&candidate.provider_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE person_match_candidates
             SET status = 'REJECTED', evidence_json = ?, updated_at = ?
             WHERE id = ? AND status = 'CONFIRMED'",
        )
        .bind(evidence_json)
        .bind(now)
        .bind(candidate_id)
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
        Ok(StoredPersonIdentityMove { previous_person_id })
    }

    pub(crate) async fn split_canonical_person_identity(
        &self,
        source_person_id: &str,
        provider: &str,
        provider_id: &str,
        display_name: &str,
        evidence_json: &str,
    ) -> Result<StoredCanonicalPerson, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let owner = self
            .query_scalar::<String>(
                "SELECT person_id FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(provider)
            .bind(provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .ok_or_else(|| {
                StorageError::Conflict(format!(
                    "provider identity '{provider}:{provider_id}' does not exist"
                ))
            })?;
        if owner != source_person_id {
            return Err(StorageError::Conflict(format!(
                "provider identity '{provider}:{provider_id}' belongs to '{owner}'"
            )));
        }
        let new_person_id = loop {
            let sequence: i64 = self
                .query_scalar("INSERT INTO person_id_sequence DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let candidate = format!("lux-{sequence:06}");
            let exists: Option<i64> = self
                .query_scalar("SELECT 1 FROM people WHERE id = ?")
                .bind(&candidate)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if exists.is_none() {
                break candidate;
            }
        };
        self.query(
            "INSERT INTO people (
                id, display_name, directory_name, normalized_name, status, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 'ACTIVE', ?, ?)",
        )
        .bind(&new_person_id)
        .bind(display_name)
        .bind(display_name)
        .bind(normalize_person_name(display_name))
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query("DELETE FROM person_identities WHERE provider = ? AND provider_id = ?")
            .bind(provider)
            .bind(provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO person_identities (
                person_id, provider, provider_id, match_method, confidence,
                evidence_json, created_at, updated_at
             ) VALUES (?, ?, ?, 'MANUAL_SPLIT', ?, ?, ?, ?)",
        )
        .bind(&new_person_id)
        .bind(provider)
        .bind(provider_id)
        .bind(Some(1.0_f64))
        .bind(evidence_json)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE person_credits SET lux_person_id = ?
             WHERE provider = ? AND person_id = ?",
        )
        .bind(&new_person_id)
        .bind(provider)
        .bind(provider_id)
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
        Ok(StoredCanonicalPerson { id: new_person_id })
    }

    pub(crate) async fn restore_canonical_person(
        &self,
        person_id: &str,
        display_name: &str,
        identities: &[(&str, &str)],
    ) -> Result<StoredCanonicalPerson, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for (provider, provider_id) in identities {
            let owner = self
                .query_scalar::<String>(
                    "SELECT person_id FROM person_identities
                     WHERE provider = ? AND provider_id = ?",
                )
                .bind(provider)
                .bind(provider_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if let Some(owner) = owner
                && owner != person_id
            {
                return Err(StorageError::Conflict(format!(
                    "provider identity '{provider}:{provider_id}' belongs to '{owner}'"
                )));
            }
        }
        self.query(
            "INSERT INTO people (
                id, display_name, directory_name, normalized_name, status, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 'ACTIVE', ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                display_name = excluded.display_name,
                directory_name = excluded.directory_name,
                normalized_name = excluded.normalized_name,
                updated_at = excluded.updated_at",
        )
        .bind(person_id)
        .bind(display_name)
        .bind(display_name)
        .bind(normalize_person_name(display_name))
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if let Some(sequence) = person_id
            .strip_prefix("lux-")
            .and_then(|value| value.parse::<i64>().ok())
        {
            self.query(
                "INSERT INTO person_id_sequence (id) VALUES (?)
                 ON CONFLICT(id) DO NOTHING",
            )
            .bind(sequence)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            if self.backend == DatabaseBackend::Postgres {
                self.query(
                    "SELECT setval(
                        pg_get_serial_sequence('person_id_sequence', 'id'),
                        COALESCE((SELECT MAX(id) FROM person_id_sequence), 1),
                        true
                    )",
                )
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }
        for (provider, provider_id) in identities {
            self.query(
                "INSERT INTO person_identities (
                    person_id, provider, provider_id, match_method, confidence,
                    evidence_json, created_at, updated_at
                 ) VALUES (?, ?, ?, 'RECOVERED_MANIFEST', ?, ?, ?, ?)
                 ON CONFLICT(provider, provider_id) DO NOTHING",
            )
            .bind(person_id)
            .bind(provider)
            .bind(provider_id)
            .bind(Some(1.0_f64))
            .bind(r#"{"method":"person-manifest"}"#)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        let row = self
            .query("SELECT id FROM people WHERE id = ?")
            .bind(person_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let stored = stored_canonical_person(row);
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(stored)
    }

    pub(crate) async fn person_manifest_restore_needed(
        &self,
        schema_version: i64,
    ) -> Result<bool, StorageError> {
        let status = self
            .query_scalar::<String>(
                "SELECT status FROM person_manifest_restore_state
                 WHERE id = 1 AND schema_version = ?",
            )
            .bind(schema_version)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(status.as_deref() != Some("COMPLETED"))
    }

    pub(crate) async fn legacy_person_migration_needed(
        &self,
        schema_version: i64,
    ) -> Result<bool, StorageError> {
        let status = self
            .query_scalar::<String>(
                "SELECT status FROM legacy_person_migration_state
                 WHERE id = 1 AND schema_version = ?",
            )
            .bind(schema_version)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(status.as_deref() != Some("COMPLETED"))
    }

    pub(crate) async fn mark_legacy_person_migration_completed(
        &self,
        schema_version: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO legacy_person_migration_state (id, status, schema_version, updated_at)
             VALUES (1, 'COMPLETED', ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                status = excluded.status,
                schema_version = excluded.schema_version,
                updated_at = excluded.updated_at",
        )
        .bind(schema_version)
        .bind(current_unix_timestamp())
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub(crate) async fn mark_person_manifest_restore_pending(
        &self,
        schema_version: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO person_manifest_restore_state (id, status, schema_version, updated_at)
             VALUES (1, 'PENDING', ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                status = excluded.status,
                schema_version = excluded.schema_version,
                updated_at = excluded.updated_at",
        )
        .bind(schema_version)
        .bind(current_unix_timestamp())
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub(crate) async fn mark_person_manifest_restore_completed(
        &self,
        schema_version: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO person_manifest_restore_state (id, status, schema_version, updated_at)
             VALUES (1, 'COMPLETED', ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                status = excluded.status,
                schema_version = excluded.schema_version,
                updated_at = excluded.updated_at",
        )
        .bind(schema_version)
        .bind(current_unix_timestamp())
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub(crate) async fn restore_canonical_person_if_manifest_changed(
        &self,
        person_id: &str,
        display_name: &str,
        identities: &[(&str, &str)],
        manifest_checksum: &str,
        manifest_schema_version: i64,
    ) -> Result<bool, StorageError> {
        let unchanged = self
            .query_scalar::<String>(
                "SELECT manifest_checksum FROM person_manifest_index_state
                 WHERE person_id = ? AND manifest_checksum = ?
                   AND manifest_schema_version = ?",
            )
            .bind(person_id)
            .bind(manifest_checksum)
            .bind(manifest_schema_version)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .is_some();
        if unchanged {
            return Ok(false);
        }

        self.restore_canonical_person(person_id, display_name, identities)
            .await?;
        self.query(
            "INSERT INTO person_manifest_index_state (
                person_id, manifest_checksum, manifest_schema_version, updated_at
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(person_id) DO UPDATE SET
                manifest_checksum = excluded.manifest_checksum,
                manifest_schema_version = excluded.manifest_schema_version,
                updated_at = excluded.updated_at",
        )
        .bind(person_id)
        .bind(manifest_checksum)
        .bind(manifest_schema_version)
        .bind(current_unix_timestamp())
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(true)
    }

    pub(crate) async fn attach_canonical_person_identity(
        &self,
        person_id: &str,
        provider: &str,
        provider_id: &str,
        match_method: &str,
        confidence: Option<f64>,
        evidence_json: &str,
    ) -> Result<StoredCanonicalPerson, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let person = self
            .query(
                "SELECT id
                 FROM people WHERE id = ?",
            )
            .bind(person_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .ok_or_else(|| {
                StorageError::Conflict(format!("canonical person '{person_id}' does not exist"))
            })?;

        self.query(
            "INSERT INTO person_identities (
                person_id, provider, provider_id, match_method, confidence,
                evidence_json, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(provider, provider_id) DO NOTHING",
        )
        .bind(person_id)
        .bind(provider)
        .bind(provider_id)
        .bind(match_method)
        .bind(confidence)
        .bind(evidence_json)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        let owner_id: String = self
            .query_scalar(
                "SELECT person_id FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(provider)
            .bind(provider_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if owner_id != person_id {
            return Err(StorageError::Conflict(format!(
                "provider identity '{provider}:{provider_id}' belongs to '{owner_id}'"
            )));
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(stored_canonical_person(person))
    }

    pub(crate) async fn list_person_credit_item_ids(
        &self,
        library_ids: &[String],
        person_type: &str,
        person_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT DISTINCT pc.item_id
             FROM person_credits pc
             JOIN media_items mi ON mi.id = pc.item_id
             LEFT JOIN person_identities pi
               ON pi.provider = pc.provider
              AND pi.provider_id = pc.person_id
             WHERE mi.library_id IN ({placeholders})
               AND mi.removed_at IS NULL
               AND pc.person_type = ?
               AND (
                   pc.person_id = ?
                   OR pc.lux_person_id = ?
                   OR pi.person_id = ?
               )
             ORDER BY pc.item_id"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        let rows = statement
            .bind(person_type)
            .bind(person_id)
            .bind(person_id)
            .bind(person_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows.into_iter().map(|row| row.get("item_id")).collect())
    }

    pub(crate) async fn list_person_credits_for_library(
        &self,
        library_id: &str,
        person_type: &str,
        options: PersonListOptions,
    ) -> Result<(Vec<StoredPersonCredit>, i64), StorageError> {
        self.list_person_credits_for_libraries(&[library_id.to_owned()], person_type, options)
            .await
    }

    pub(crate) async fn list_person_credits_for_libraries(
        &self,
        library_ids: &[String],
        person_type: &str,
        options: PersonListOptions,
    ) -> Result<(Vec<StoredPersonCredit>, i64), StorageError> {
        if library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let person_group = "COALESCE(
            NULLIF(pc.lux_person_id, ''),
            NULLIF(pi.person_id, ''),
            pc.provider || ':' || pc.person_id
        )";
        let person_sort_order = person_sort_order(options.sort_by, options.descending);
        let recursive_clause = if options.recursive {
            String::new()
        } else {
            format!(" AND (mi.parent_id IS NULL OR mi.parent_id IN ({placeholders}))")
        };
        let count_query = format!(
            "SELECT COUNT(*) FROM (
                 SELECT {person_group}
                 FROM person_credits pc
                 JOIN media_items mi ON mi.id = pc.item_id
                 LEFT JOIN person_identities pi
                   ON pi.provider = pc.provider
                  AND pi.provider_id = pc.person_id
                 WHERE mi.library_id IN ({placeholders})
                   AND mi.removed_at IS NULL
                   {recursive_clause}
                   AND pc.person_type = ?
                 GROUP BY {person_group}
             )"
        );
        let mut count_statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_query));
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        if !options.recursive {
            for library_id in library_ids {
                count_statement = count_statement.bind(library_id);
            }
        }
        let total: i64 = count_statement
            .bind(person_type)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let list_query = format!(
            "WITH eligible_person_credits AS (
                 SELECT pc.item_id,
                        pc.person_type,
                        pc.provider,
                        pc.person_id,
                        pc.role,
                        mi.added_at,
                        {person_group} AS person_group
                 FROM person_credits pc
                 JOIN media_items mi ON mi.id = pc.item_id
                 LEFT JOIN person_identities pi
                   ON pi.provider = pc.provider
                  AND pi.provider_id = pc.person_id
                 WHERE mi.library_id IN ({placeholders})
                   AND mi.removed_at IS NULL
                   {recursive_clause}
                   AND pc.person_type = ?
             ), ranked_person_credits AS (
                 SELECT eligible_person_credits.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY person_group
                            ORDER BY item_id ASC,
                                     provider ASC,
                                     person_id ASC,
                                     role ASC
                        ) AS representative_rank,
                        MIN(added_at) OVER (PARTITION BY person_group) AS date_created
                 FROM eligible_person_credits
             )
             SELECT representative.item_id,
                    representative.person_id,
                    representative.lux_person_id,
                    representative.provider,
                    representative.person_name,
                    representative.role,
                    representative.date_created,
                    representative.biography,
                    representative.birthday,
                    representative.deathday,
                    representative.known_for_department,
                    representative.place_of_birth,
                    representative.provider_ids_json,
                    representative.genres_json,
                    representative.tags_json,
                    representative.production_locations_json,
                    representative.premiere_date,
                    representative.production_year,
                    representative.taglines_json
             FROM (
                 SELECT pc.item_id,
                        pc.person_id,
                        pc.lux_person_id,
                        pc.provider,
                        pc.person_name,
                        pc.role,
                        ranked.date_created,
                        pc.biography,
                        pc.birthday,
                        pc.deathday,
                        pc.known_for_department,
                        pc.place_of_birth,
                        pc.provider_ids_json,
                        pc.genres_json,
                        pc.tags_json,
                        pc.production_locations_json,
                        pc.premiere_date,
                        pc.production_year,
                        pc.taglines_json
                 FROM ranked_person_credits ranked
                 JOIN person_credits pc
                   ON pc.item_id = ranked.item_id
                  AND pc.person_type = ranked.person_type
                  AND pc.provider = ranked.provider
                  AND pc.person_id = ranked.person_id
                  AND pc.role = ranked.role
                 WHERE ranked.representative_rank = 1
             ) AS representative
             ORDER BY {person_sort_order}
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = self.query(sqlx::AssertSqlSafe(list_query));
        for library_id in library_ids {
            list_statement = list_statement.bind(library_id);
        }
        if !options.recursive {
            for library_id in library_ids {
                list_statement = list_statement.bind(library_id);
            }
        }
        let rows = list_statement
            .bind(person_type)
            .bind(options.limit)
            .bind(options.offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(stored_person_credit)
            .collect();
        Ok((rows, total))
    }

    pub(crate) async fn search_person_credits_for_libraries(
        &self,
        library_ids: &[String],
        person_type: &str,
        query: &str,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<StoredPersonCredit>, i64), StorageError> {
        if library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let like_query = format!("%{escaped}%");
        let person_group = "COALESCE(
            NULLIF(pc.lux_person_id, ''),
            NULLIF(pi.person_id, ''),
            pc.provider || ':' || pc.person_id
        )";
        let count_query = format!(
            "SELECT COUNT(*) FROM (
                 SELECT {person_group}
                 FROM person_credits pc
                 JOIN media_items mi ON mi.id = pc.item_id
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN person_identities pi
                   ON pi.provider = pc.provider
                  AND pi.provider_id = pc.person_id
                 WHERE mi.library_id IN ({placeholders})
                   AND mi.removed_at IS NULL
                   {CATALOG_VISIBLE_PREDICATE}
                   AND pc.person_type = ?
                   AND pc.person_name LIKE ? ESCAPE '\\'
                 GROUP BY {person_group}
             )"
        );
        let mut count_statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_query));
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        let total = count_statement
            .bind(person_type)
            .bind(&like_query)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let list_query = format!(
            "WITH eligible_person_credits AS (
                 SELECT pc.item_id,
                        pc.person_type,
                        pc.provider,
                        pc.person_id,
                        pc.role,
                        mi.added_at,
                        {person_group} AS person_group
                 FROM person_credits pc
                 JOIN media_items mi ON mi.id = pc.item_id
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN person_identities pi
                   ON pi.provider = pc.provider
                  AND pi.provider_id = pc.person_id
                 WHERE mi.library_id IN ({placeholders})
                   AND mi.removed_at IS NULL
                   {CATALOG_VISIBLE_PREDICATE}
                   AND pc.person_type = ?
                   AND pc.person_name LIKE ? ESCAPE '\\'
             ), ranked_person_credits AS (
                 SELECT eligible_person_credits.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY person_group
                            ORDER BY item_id ASC,
                                     provider ASC,
                                     person_id ASC,
                                     role ASC
                        ) AS representative_rank,
                        MIN(added_at) OVER (PARTITION BY person_group) AS date_created
                 FROM eligible_person_credits
             )
             SELECT representative.item_id,
                    representative.person_id,
                    representative.lux_person_id,
                    representative.provider,
                    representative.person_name,
                    representative.role,
                    representative.date_created,
                    representative.biography,
                    representative.birthday,
                    representative.deathday,
                    representative.known_for_department,
                    representative.place_of_birth,
                    representative.provider_ids_json,
                    representative.genres_json,
                    representative.tags_json,
                    representative.production_locations_json,
                    representative.premiere_date,
                    representative.production_year,
                    representative.taglines_json
             FROM (
                 SELECT pc.item_id,
                        pc.person_id,
                        pc.lux_person_id,
                        pc.provider,
                        pc.person_name,
                        pc.role,
                        ranked.date_created,
                        pc.biography,
                        pc.birthday,
                        pc.deathday,
                        pc.known_for_department,
                        pc.place_of_birth,
                        pc.provider_ids_json,
                        pc.genres_json,
                        pc.tags_json,
                        pc.production_locations_json,
                        pc.premiere_date,
                        pc.production_year,
                        pc.taglines_json
                 FROM ranked_person_credits ranked
                 JOIN person_credits pc
                   ON pc.item_id = ranked.item_id
                  AND pc.person_type = ranked.person_type
                  AND pc.provider = ranked.provider
                  AND pc.person_id = ranked.person_id
                  AND pc.role = ranked.role
                 WHERE ranked.representative_rank = 1
             ) AS representative
             ORDER BY CASE WHEN LOWER(representative.person_name) = LOWER(?) THEN 0 ELSE 1 END,
                      LOWER(representative.person_name) ASC,
                      representative.provider ASC,
                      representative.person_id ASC
             LIMIT ? OFFSET ?"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(list_query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        let rows = statement
            .bind(person_type)
            .bind(&like_query)
            .bind(query.trim())
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .bind(offset.max(0))
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(stored_person_credit)
            .collect();
        Ok((rows, total))
    }

    pub(crate) async fn find_person_credits_for_libraries(
        &self,
        library_ids: &[String],
        person_type: &str,
        person_id: &str,
    ) -> Result<Vec<StoredPersonCredit>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let person_group = "COALESCE(
            NULLIF(pc.lux_person_id, ''),
            NULLIF(pi.person_id, ''),
            pc.provider || ':' || pc.person_id
        )";
        let query = format!(
            "SELECT MIN(pc.item_id) AS item_id,
                    MIN(pc.person_id) AS person_id,
                    MIN(pc.lux_person_id) AS lux_person_id,
                    MIN(pc.provider) AS provider,
                    MIN(pc.person_name) AS person_name,
                    MIN(pc.role) AS role,
                    MIN(mi.added_at) AS date_created,
                    MIN(pc.biography) AS biography,
                    MIN(pc.birthday) AS birthday,
                    MIN(pc.deathday) AS deathday,
                    MIN(pc.known_for_department) AS known_for_department,
                    MIN(pc.place_of_birth) AS place_of_birth,
                    MIN(pc.provider_ids_json) AS provider_ids_json,
                    MIN(pc.genres_json) AS genres_json,
                    MIN(pc.tags_json) AS tags_json,
                    MIN(pc.production_locations_json) AS production_locations_json,
                    MIN(pc.premiere_date) AS premiere_date,
                    MIN(pc.production_year) AS production_year,
                    MIN(pc.taglines_json) AS taglines_json
             FROM person_credits pc
             JOIN media_items mi ON mi.id = pc.item_id
             LEFT JOIN person_identities pi
               ON pi.provider = pc.provider
              AND pi.provider_id = pc.person_id
             WHERE mi.library_id IN ({placeholders})
               AND mi.removed_at IS NULL
               AND pc.person_type = ?
               AND (
                   pc.person_id = ?
                   OR pc.lux_person_id = ?
                   OR pi.person_id = ?
               )
             GROUP BY {person_group}
             ORDER BY CASE WHEN MIN(pc.provider) = '' THEN 1 ELSE 0 END,
                      MIN(pc.provider) ASC,
                      MIN(pc.person_id) ASC"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        let rows = statement
            .bind(person_type)
            .bind(person_id)
            .bind(person_id)
            .bind(person_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(stored_person_credit)
            .collect();
        Ok(rows)
    }

    pub(crate) async fn find_person_credits_for_libraries_by_name(
        &self,
        library_ids: &[String],
        person_type: &str,
        person_name: &str,
    ) -> Result<Vec<StoredPersonCredit>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let person_group = "COALESCE(
            NULLIF(pc.lux_person_id, ''),
            NULLIF(pi.person_id, ''),
            pc.provider || ':' || pc.person_id
        )";
        let query = format!(
            "SELECT MIN(pc.item_id) AS item_id,
                    MIN(pc.person_id) AS person_id,
                    MIN(pc.lux_person_id) AS lux_person_id,
                    MIN(pc.provider) AS provider,
                    MIN(pc.person_name) AS person_name,
                    MIN(pc.role) AS role,
                    MIN(mi.added_at) AS date_created,
                    MIN(pc.biography) AS biography,
                    MIN(pc.birthday) AS birthday,
                    MIN(pc.deathday) AS deathday,
                    MIN(pc.known_for_department) AS known_for_department,
                    MIN(pc.place_of_birth) AS place_of_birth,
                    MIN(pc.provider_ids_json) AS provider_ids_json,
                    MIN(pc.genres_json) AS genres_json,
                    MIN(pc.tags_json) AS tags_json,
                    MIN(pc.production_locations_json) AS production_locations_json,
                    MIN(pc.premiere_date) AS premiere_date,
                    MIN(pc.production_year) AS production_year,
                    MIN(pc.taglines_json) AS taglines_json
             FROM person_credits pc
             JOIN media_items mi ON mi.id = pc.item_id
             LEFT JOIN person_identities pi
               ON pi.provider = pc.provider
              AND pi.provider_id = pc.person_id
             WHERE mi.library_id IN ({placeholders})
               AND mi.removed_at IS NULL
               AND pc.person_type = ?
               AND pc.person_name = ?
             GROUP BY {person_group}
             ORDER BY CASE WHEN MIN(pc.provider) = '' THEN 1 ELSE 0 END,
                      MIN(pc.provider) ASC,
                      MIN(pc.person_id) ASC"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        let rows = statement
            .bind(person_type)
            .bind(person_name)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(stored_person_credit)
            .collect();
        Ok(rows)
    }

    pub(crate) async fn list_person_index_item_ids(
        &self,
        library_id: &str,
        after_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        let sql = if after_id.is_some() {
            "SELECT id FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
               AND id > ?
             ORDER BY id LIMIT ?"
        } else {
            "SELECT id FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
             ORDER BY id LIMIT ?"
        };
        let mut query = self.query_scalar::<String>(sql).bind(library_id);
        if let Some(after_id) = after_id {
            query = query.bind(after_id);
        }
        query
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn count_person_index_items(
        &self,
        library_id: &str,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT COUNT(*) FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}
