use super::*;

impl Database {
    pub(super) async fn apply_scan_manifest_discovery_positive_indexes_in_transaction<'a, 'b>(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        commit: ManifestDiscoveryPositiveIndexCommit<'a, 'b>,
    ) -> Result<ManifestDiscoveryPositiveIndexResult<'a>, StorageError> {
        let ManifestDiscoveryPositiveIndexCommit {
            job_id,
            library_id,
            library_root_id,
            generation,
            positives,
            observations,
        } = commit;
        if positives.is_empty() {
            return Ok(ManifestDiscoveryPositiveIndexResult::default());
        }

        let observations = observations
            .iter()
            .map(|observation| (observation.relative_path.as_str(), *observation))
            .collect::<HashMap<_, _>>();
        let mut add_filesystem_entries = Vec::new();
        for positive in positives
            .iter()
            .filter(|positive| positive.delta_kind == "ADD")
        {
            let observation = observations
                .get(positive.relative_path.as_str())
                .ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest add is missing its filesystem observation".to_owned(),
                    )
                })?;
            let fingerprint = observation.fingerprint.as_slice();
            add_filesystem_entries.push(NewScanManifestFilesystemEntry {
                id: manifest_positive_file_filesystem_entry_id(&positive.file),
                relative_path: &positive.relative_path,
                size: observation.size,
                modified_at: observation.modified_at,
                inode: observation.inode,
                fingerprint,
                last_seen_change_kind: Some(
                    if matches!(&positive.file, NewScanManifestIndexedFile::Sidecar(_)) {
                        "SIDECAR"
                    } else {
                        "NEW"
                    },
                ),
            });
        }
        let add_filesystem_claim_started = Instant::now();
        let claimed_add_paths = self
            .claim_manifest_add_filesystem_entries_in_transaction(
                transaction,
                library_root_id,
                generation,
                &add_filesystem_entries,
            )
            .await?;
        record_manifest_storage_stage(
            "positive_add_filesystem_claim",
            add_filesystem_claim_started,
            add_filesystem_entries.len(),
            add_filesystem_entries.len(),
            0,
        );

        let claimed_movie_files = positives
            .iter()
            .filter(|positive| {
                positive.delta_kind == "ADD" && claimed_add_paths.contains(&positive.relative_path)
            })
            .filter_map(|positive| match &positive.file {
                NewScanManifestIndexedFile::Movie(file) => Some(file.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let claimed_episode_files = positives
            .iter()
            .filter(|positive| {
                positive.delta_kind == "ADD" && claimed_add_paths.contains(&positive.relative_path)
            })
            .filter_map(|positive| match &positive.file {
                NewScanManifestIndexedFile::Episode(file) => Some(file.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();

        let mut result = ManifestDiscoveryPositiveIndexResult::default();
        let movie_materialization_started = Instant::now();
        result.created_items = self
            .insert_movie_files_without_filesystem_entries_in_transaction(
                transaction,
                library_id,
                library_root_id,
                generation,
                &claimed_movie_files,
            )
            .await?;
        record_manifest_storage_stage(
            "positive_add_movie_materialization",
            movie_materialization_started,
            claimed_movie_files.len(),
            claimed_movie_files.len(),
            0,
        );
        let episode_materialization_started = Instant::now();
        result.created_items = result.created_items.saturating_add(
            self.insert_episode_files_without_filesystem_entries_in_transaction(
                transaction,
                library_id,
                library_root_id,
                generation,
                &claimed_episode_files,
            )
            .await?,
        );
        record_manifest_storage_stage(
            "positive_add_episode_materialization",
            episode_materialization_started,
            claimed_episode_files.len(),
            claimed_episode_files.len(),
            0,
        );

        let mut changed_sidecar_paths = Vec::new();
        let mut existing_updates = Vec::new();
        let positive_change_application_started = Instant::now();

        for positive in positives {
            match positive.delta_kind.as_str() {
                "ADD" => {
                    if !claimed_add_paths.contains(&positive.relative_path) {
                        continue;
                    }
                    match &positive.file {
                        NewScanManifestIndexedFile::Movie(_)
                        | NewScanManifestIndexedFile::Episode(_) => {}
                        NewScanManifestIndexedFile::Unresolved(file) => {
                            self.materialize_manifest_unresolved_file_after_filesystem_insert_in_transaction(
                                transaction,
                                library_id,
                                library_root_id,
                                file,
                            )
                            .await?;
                            result.created_items = result.created_items.saturating_add(1);
                        }
                        NewScanManifestIndexedFile::Sidecar(_) => {
                            changed_sidecar_paths.push(positive.relative_path.clone());
                        }
                    }
                    if is_file_change_only_sidecar_path(&positive.relative_path) {
                        record_manifest_file_change_applied(&mut result, positive);
                    } else {
                        record_manifest_positive_applied(&mut result, positive);
                    }
                }
                "CHANGE" | "REAPPEARED" => {
                    let Some(filesystem_entry_id) = positive.base_filesystem_entry_id.as_deref()
                    else {
                        return Err(StorageError::Conflict(
                            "manifest changed file is missing its filesystem baseline".to_owned(),
                        ));
                    };
                    let observation = observations
                        .get(positive.relative_path.as_str())
                        .ok_or_else(|| {
                            StorageError::Conflict(
                                "manifest changed file is missing its observation".to_owned(),
                            )
                        })?;
                    let expected_missing = positive.delta_kind == "REAPPEARED";
                    match &positive.file {
                        NewScanManifestIndexedFile::Movie(file) => {
                            existing_updates.push(ManifestExistingFileUpdate {
                                filesystem_entry_id,
                                library_root_id,
                                relative_path: &positive.relative_path,
                                base_fingerprint: positive.base_fingerprint.as_deref(),
                                expected_missing,
                                size: observation.size,
                                modified_at: observation.modified_at,
                                inode: observation.inode,
                                fingerprint: observation.fingerprint.as_slice(),
                                generation,
                                last_seen_change_kind: Some("CHANGED"),
                                source_kind: &file.source_kind,
                                edition_name: file.edition_name.as_deref(),
                                quality_label: file.quality_label.as_deref(),
                                container: &file.container,
                                external_url: file.external_url.as_deref(),
                                strm_target_kind: file.strm_target_kind.as_deref(),
                            });
                        }
                        NewScanManifestIndexedFile::Episode(file) => {
                            existing_updates.push(ManifestExistingFileUpdate {
                                filesystem_entry_id,
                                library_root_id,
                                relative_path: &positive.relative_path,
                                base_fingerprint: positive.base_fingerprint.as_deref(),
                                expected_missing,
                                size: observation.size,
                                modified_at: observation.modified_at,
                                inode: observation.inode,
                                fingerprint: observation.fingerprint.as_slice(),
                                generation,
                                last_seen_change_kind: Some("CHANGED"),
                                source_kind: &file.source_kind,
                                edition_name: file.edition_name.as_deref(),
                                quality_label: file.quality_label.as_deref(),
                                container: &file.container,
                                external_url: file.external_url.as_deref(),
                                strm_target_kind: file.strm_target_kind.as_deref(),
                            });
                        }
                        NewScanManifestIndexedFile::Unresolved(file) => {
                            existing_updates.push(ManifestExistingFileUpdate {
                                filesystem_entry_id,
                                library_root_id,
                                relative_path: &positive.relative_path,
                                base_fingerprint: positive.base_fingerprint.as_deref(),
                                expected_missing,
                                size: observation.size,
                                modified_at: observation.modified_at,
                                inode: observation.inode,
                                fingerprint: observation.fingerprint.as_slice(),
                                generation,
                                last_seen_change_kind: Some("CHANGED"),
                                source_kind: &file.source_kind,
                                edition_name: None,
                                quality_label: None,
                                container: &file.container,
                                external_url: file.external_url.as_deref(),
                                strm_target_kind: file.strm_target_kind.as_deref(),
                            });
                        }
                        NewScanManifestIndexedFile::Sidecar(_) => {
                            let applied = self
                                .apply_manifest_existing_sidecar_in_transaction(
                                    transaction,
                                    library_root_id,
                                    generation,
                                    positive,
                                    observation,
                                    expected_missing,
                                )
                                .await?;
                            if applied {
                                changed_sidecar_paths.push(positive.relative_path.clone());
                                if is_file_change_only_sidecar_path(&positive.relative_path) {
                                    record_manifest_file_change_applied(&mut result, positive);
                                } else {
                                    record_manifest_positive_applied(&mut result, positive);
                                }
                            }
                        }
                    }
                }
                _ => {
                    return Err(StorageError::Conflict(
                        "manifest positive index has an unknown delta kind".to_owned(),
                    ));
                }
            }
        }

        let applied_existing_ids = self
            .apply_manifest_existing_files_batch_in_transaction(transaction, &existing_updates)
            .await?;
        for positive in positives.iter().filter(|positive| {
            matches!(positive.delta_kind.as_str(), "CHANGE" | "REAPPEARED")
                && !matches!(&positive.file, NewScanManifestIndexedFile::Sidecar(_))
        }) {
            if positive
                .base_filesystem_entry_id
                .as_deref()
                .is_some_and(|id| applied_existing_ids.contains(id))
            {
                record_manifest_positive_applied(&mut result, positive);
            }
        }
        record_manifest_storage_stage(
            "positive_change_application",
            positive_change_application_started,
            positives.len(),
            positives.len(),
            0,
        );

        let mut file_change_events = Vec::new();
        for path in &changed_sidecar_paths {
            if !is_file_change_only_sidecar_path(path) {
                continue;
            }
            let positive = positives
                .iter()
                .find(|positive| positive.relative_path == *path)
                .ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest sidecar event has no matching positive index".to_owned(),
                    )
                })?;
            let change_kind = if matches!(positive.delta_kind.as_str(), "ADD" | "REAPPEARED") {
                "ADD"
            } else {
                "CHANGE"
            };
            file_change_events.push((path.as_str(), change_kind));
        }
        self.record_manifest_file_changes_in_transaction(
            transaction,
            job_id,
            library_root_id,
            &file_change_events,
        )
        .await?;

        let legacy_changed_sidecar_paths = changed_sidecar_paths
            .iter()
            .filter(|path| !is_file_change_only_sidecar_path(path))
            .collect::<Vec<_>>();
        let sidecar_directories = prune_sidecar_directories(
            legacy_changed_sidecar_paths
                .iter()
                .filter_map(|path| {
                    Path::new(path)
                        .parent()
                        .and_then(|parent| parent.to_str())
                        .map(|parent| {
                            if parent.is_empty() {
                                ".".to_owned()
                            } else {
                                parent.to_owned()
                            }
                        })
                })
                .collect(),
        );
        let sidecar_target_registration_started = Instant::now();
        result.metadata_targets_changed |= self
            .record_scan_job_sidecar_targets_in_transaction(
                transaction,
                job_id,
                library_root_id,
                &sidecar_directories,
            )
            .await?;
        record_manifest_storage_stage(
            "positive_sidecar_target_registration",
            sidecar_target_registration_started,
            sidecar_directories.len(),
            0,
            sidecar_directories.len(),
        );
        Ok(result)
    }

    pub(super) async fn record_manifest_file_changes_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        library_root_id: &str,
        changes: &[(&str, &str)],
    ) -> Result<bool, StorageError> {
        if changes.is_empty() {
            return Ok(false);
        }
        let chunk_size = (super::manifest_path_query_chunk_size(self.backend()) / 4).max(1);
        let mut changed = false;
        for chunk in changes.chunks(chunk_size) {
            let values = std::iter::repeat_n("(?, ?, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO scan_job_file_changes (
                     job_id, library_root_id, relative_path, change_kind
                 ) VALUES {values}
                 ON CONFLICT(job_id, library_root_id, relative_path) DO UPDATE SET
                     change_kind = excluded.change_kind,
                     processing_state = 'PENDING', error = NULL,
                     revision = scan_job_file_changes.revision + 1,
                     updated_at = unixepoch()
                 WHERE scan_job_file_changes.change_kind <> excluded.change_kind
                    OR scan_job_file_changes.processing_state <> 'PENDING'
                    OR scan_job_file_changes.error IS NOT NULL"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (relative_path, change_kind) in chunk {
                statement = statement
                    .bind(job_id)
                    .bind(library_root_id)
                    .bind(relative_path)
                    .bind(change_kind);
            }
            let result = statement
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            changed |= result.rows_affected() > 0;
        }
        Ok(changed)
    }
}

pub(super) fn is_file_change_only_sidecar_path(relative_path: &str) -> bool {
    let path = Path::new(relative_path);
    let is_external_subtitle = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "srt" | "ass" | "ssa" | "vtt" | "sub" | "sup"
            )
        });
    let is_media_info = path
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| file_name.to_ascii_lowercase().ends_with("-mediainfo.json"));
    let is_danmaku_xml = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"));
    let is_nfo = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("nfo"));
    is_external_subtitle || is_media_info || is_danmaku_xml || is_nfo
}

fn record_manifest_file_change_applied<'a>(
    result: &mut ManifestDiscoveryPositiveIndexResult<'a>,
    positive: &'a NewScanManifestPositiveIndex,
) {
    result.indexed_paths.push(positive.relative_path.as_str());
    result.applied_count = result.applied_count.saturating_add(1);
    match positive.delta_kind.as_str() {
        "ADD" => result.add_count = result.add_count.saturating_add(1),
        "CHANGE" => result.change_count = result.change_count.saturating_add(1),
        "REAPPEARED" => result.reappeared_count = result.reappeared_count.saturating_add(1),
        _ => {}
    }
}
