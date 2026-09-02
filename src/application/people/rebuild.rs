use super::*;

impl PeopleService {
    pub(super) async fn find_person_manifest_path(
        &self,
        person_id: &str,
        display_name: &str,
    ) -> Result<PathBuf, PeopleError> {
        let root = metadata_root(&self.config_dir)
            .join(LEGACY_PEOPLE_DIR)
            .join("person");
        let mut initials = match fs::read_dir(&root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(path) = self
                    .find_person_manifest_path_from_index(person_id, display_name)
                    .await?
                {
                    return Ok(path);
                }
                return lux_person_directory(&self.config_dir, display_name, person_id)
                    .map_err(PeopleError::from)
                    .map(|path| path.join(PERSON_MANIFEST));
            }
            Err(source) => return Err(PeopleError::Io { path: root, source }),
        };
        while let Some(initial) = initials
            .next_entry()
            .await
            .map_err(|source| PeopleError::Io {
                path: root.clone(),
                source,
            })?
        {
            let initial_path = initial.path();
            if safe_metadata(&initial_path)
                .await?
                .is_none_or(|metadata| !metadata.is_dir())
            {
                continue;
            }
            let mut persons =
                fs::read_dir(&initial_path)
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: initial_path.clone(),
                        source,
                    })?;
            while let Some(person) =
                persons
                    .next_entry()
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: initial_path.clone(),
                        source,
                    })?
            {
                let candidate_path = person.path().join(PERSON_MANIFEST);
                let Some(bytes) = read_people_file(&candidate_path).await? else {
                    continue;
                };
                let Ok(manifest) = serde_json::from_slice::<PersonManifest>(&bytes) else {
                    continue;
                };
                if manifest.lux_person_id == person_id {
                    return Ok(candidate_path);
                }
            }
        }
        if let Some(path) = self
            .find_person_manifest_path_from_index(person_id, display_name)
            .await?
        {
            return Ok(path);
        }
        lux_person_directory(&self.config_dir, display_name, person_id)
            .map_err(PeopleError::from)
            .map(|path| path.join(PERSON_MANIFEST))
    }

    pub(super) async fn find_person_manifest_path_from_index(
        &self,
        person_id: &str,
        display_name: &str,
    ) -> Result<Option<PathBuf>, PeopleError> {
        let index_root = people_index_directory(&self.config_dir);
        let mut entries = match fs::read_dir(&index_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(PeopleError::Io {
                    path: index_root,
                    source,
                });
            }
        };
        let direct_name = format!("{person_id}.json");
        let provider_name_suffix = format!("-{person_id}.json");
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| PeopleError::Io {
                path: index_root.clone(),
                source,
            })?
        {
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if file_name != direct_name && !file_name.ends_with(&provider_name_suffix) {
                continue;
            }
            let path = entry.path();
            let Some(bytes) = read_people_file(&path).await? else {
                continue;
            };
            let Ok(index) = serde_json::from_slice::<StoredPersonIndex>(&bytes) else {
                continue;
            };
            let Some(person_key) = index
                .person_key
                .as_deref()
                .filter(|person_key| person_key.starts_with("lux-"))
            else {
                continue;
            };
            return lux_person_directory(&self.config_dir, display_name, person_key)
                .map(|path| Some(path.join(PERSON_MANIFEST)))
                .map_err(PeopleError::from);
        }
        Ok(None)
    }

    pub(crate) fn schedule_person_index_rebuild(&self) {
        let service = self.clone();
        let coordinator = self.rebuild_coordinator.clone();
        tokio::spawn(async move {
            if !coordinator.begin().await {
                return;
            }
            loop {
                match service.rebuild_person_credit_index().await {
                    Ok(rebuilt_items) => {
                        tracing::info!(rebuilt_items, "person credit index rebuild completed");
                    }
                    Err(error) => {
                        tracing::error!(%error, "person credit index rebuild failed");
                    }
                }
                if !coordinator.finish().await {
                    break;
                }
            }
        });
    }

    pub async fn rebuild_person_credit_index(&self) -> Result<usize, PeopleError> {
        let _rebuild_guard = self.rebuild_lock.lock().await;
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let should_migrate_legacy_people = database
            .legacy_person_migration_needed(LEGACY_PERSON_MIGRATION_SCHEMA_VERSION)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        if should_migrate_legacy_people {
            let restored_legacy_people = self.restore_legacy_person_directories(database).await?;
            if restored_legacy_people > 0 {
                tracing::info!(restored_legacy_people, "legacy people directories migrated");
            }
            database
                .mark_legacy_person_migration_completed(LEGACY_PERSON_MIGRATION_SCHEMA_VERSION)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
        }

        let should_restore_people = database
            .person_manifest_restore_needed(PERSON_MANIFEST_SCHEMA_VERSION as i64)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        if should_restore_people {
            let restore_report = self.restore_person_manifests(database).await?;
            if restore_report.restored > 0 {
                tracing::info!(
                    restored_people = restore_report.restored,
                    "canonical people manifests restored"
                );
            }
            if restore_report.failed {
                database
                    .mark_person_manifest_restore_pending(PERSON_MANIFEST_SCHEMA_VERSION as i64)
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?;
            } else {
                database
                    .mark_person_manifest_restore_completed(PERSON_MANIFEST_SCHEMA_VERSION as i64)
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?;
            }
        }
        let restored_match_candidates = self
            .restore_person_match_candidate_snapshots(database)
            .await?;
        if restored_match_candidates > 0 {
            tracing::info!(
                restored_match_candidates,
                "person match candidate snapshots restored"
            );
        }
        let replayed_decisions = self.replay_person_decision_operations(database).await?;
        if replayed_decisions > 0 {
            tracing::info!(replayed_decisions, "person decision operations replayed");
        }
        let library_metadata_root = metadata_root(&self.config_dir).join("library");
        match fs::metadata(&library_metadata_root).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(PeopleError::Io {
                    path: library_metadata_root,
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "metadata library root is not a directory",
                    ),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %library_metadata_root.display(),
                    "skipping people index rebuild because metadata library root is missing"
                );
                return Ok(0);
            }
            Err(source) => {
                return Err(PeopleError::Io {
                    path: library_metadata_root,
                    source,
                });
            }
        }
        let jobs = database
            .sync_person_index_rebuild_jobs(PERSON_INDEX_REBUILD_SCHEMA_VERSION)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let mut rebuilt_items = 0;
        for job in jobs {
            let run_token = Uuid::now_v7().to_string();
            let force_rebuild = job.cursor_id.is_none() && job.processed_count == 0;
            if !database
                .claim_person_index_rebuild_job(&job.library_id, &run_token)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
            {
                continue;
            }
            match self
                .run_person_index_rebuild_job(database, &job, &run_token, force_rebuild)
                .await
            {
                Ok(processed) => {
                    database
                        .finish_person_index_rebuild_job(
                            &job.library_id,
                            &run_token,
                            "COMPLETED",
                            None,
                        )
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?;
                    rebuilt_items += processed;
                }
                Err(error) => {
                    let detail = error.to_string();
                    let _ = database
                        .finish_person_index_rebuild_job(
                            &job.library_id,
                            &run_token,
                            "FAILED",
                            Some(&detail),
                        )
                        .await;
                    return Err(error);
                }
            }
        }
        Ok(rebuilt_items)
    }

    pub(crate) async fn queue_person_index_rebuild(
        &self,
        library_id: &str,
    ) -> Result<bool, PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let queued = database
            .request_person_index_rebuild_job(library_id, PERSON_INDEX_REBUILD_SCHEMA_VERSION)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        if queued {
            database
                .mark_person_manifest_restore_pending(PERSON_MANIFEST_SCHEMA_VERSION as i64)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
        }
        Ok(queued)
    }

    pub(crate) async fn cancel_person_index_rebuild(
        &self,
        library_id: &str,
    ) -> Result<bool, PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        database
            .request_person_index_rebuild_job_cancel(library_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))
    }

    pub(crate) async fn list_person_index_rebuild_jobs(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredPersonIndexRebuildJob>, PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        database
            .list_person_index_rebuild_jobs(offset, limit)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))
    }

    pub(crate) async fn get_person_index_rebuild_job(
        &self,
        library_id: &str,
    ) -> Result<Option<StoredPersonIndexRebuildJob>, PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        database
            .get_person_index_rebuild_job(library_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))
    }

    pub(crate) async fn count_person_index_rebuild_jobs(&self) -> Result<i64, PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        database
            .count_person_index_rebuild_jobs()
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))
    }

    pub(super) async fn run_person_index_rebuild_job(
        &self,
        database: &Database,
        job: &StoredPersonIndexRebuildJob,
        run_token: &str,
        force_rebuild: bool,
    ) -> Result<usize, PeopleError> {
        let total_count = database
            .count_person_index_items(&job.library_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let mut cursor_id = job.cursor_id.clone();
        let mut processed_count = job.processed_count;
        let initial_processed_count = processed_count;
        loop {
            if database
                .person_index_rebuild_job_cancel_requested(&job.library_id, run_token)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
            {
                break;
            }
            let item_ids = database
                .list_person_index_item_ids(
                    &job.library_id,
                    cursor_id.as_deref(),
                    PERSON_INDEX_REBUILD_BATCH_SIZE,
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
            if item_ids.is_empty() {
                break;
            }
            let mut pending = Vec::new();
            let mut identities = BTreeSet::new();
            for item_id in &item_ids {
                let relation = match self.read_person_relation_with_path(item_id).await {
                    Ok(relation) => relation,
                    Err(PeopleError::Serialization(message)) => {
                        tracing::warn!(item_id, %message, "skipping malformed people relation during index rebuild");
                        processed_count += 1;
                        cursor_id = Some(item_id.clone());
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let Some((relation_path, relation)) = relation else {
                    let cleared = database
                        .clear_person_credits(item_id)
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?;
                    if cleared > 0 {
                        tracing::debug!(
                            item_id,
                            cleared,
                            "cleared person credits because relation snapshot is missing"
                        );
                    }
                    processed_count += 1;
                    cursor_id = Some(item_id.clone());
                    continue;
                };
                if relation.source_root.is_some()
                    && relation.source_relative_path.is_some()
                    && !self
                        .relation_has_matching_media_source(database, &relation)
                        .await?
                {
                    if self
                        .quarantine_person_relation_snapshot(&relation_path)
                        .await?
                    {
                        tracing::debug!(
                            item_id,
                            "quarantined stale people relation during index rebuild"
                        );
                    }
                    let cleared = database
                        .clear_person_credits(item_id)
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?;
                    if cleared > 0 {
                        tracing::debug!(
                            item_id,
                            cleared,
                            "cleared person credits because relation snapshot is stale"
                        );
                    }
                    processed_count += 1;
                    cursor_id = Some(item_id.clone());
                    continue;
                }
                if !force_rebuild
                    && database
                        .person_index_item_state_is_current(
                            item_id,
                            relation.source_fingerprint.as_deref(),
                        )
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?
                {
                    processed_count += 1;
                    cursor_id = Some(item_id.clone());
                    continue;
                }
                for actor in relation.actors.iter().take(MAX_ACTORS) {
                    if actor.name.trim().is_empty() {
                        continue;
                    }
                    identities.extend(
                        actor
                            .identities
                            .iter()
                            .map(|identity| (identity.provider.clone(), identity.id.clone())),
                    );
                }
                pending.push((item_id.clone(), relation));
                processed_count += 1;
                cursor_id = Some(item_id.clone());
            }

            let identity_lookup = database
                .find_canonical_people_by_identities(&identities.into_iter().collect::<Vec<_>>())
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
                .into_iter()
                .map(|(provider, provider_id, person_id)| ((provider, provider_id), person_id))
                .collect::<BTreeMap<_, _>>();
            for (item_id, relation) in pending {
                let credits =
                    self.person_credits_from_relation_with_lookup(&relation, &identity_lookup);
                database
                    .replace_person_credits_with_fingerprint(
                        &item_id,
                        &credits,
                        relation.source_fingerprint.as_deref(),
                    )
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?;
            }
            if let Some(cursor_id) = cursor_id.as_deref()
                && database
                    .update_person_index_rebuild_progress(
                        &job.library_id,
                        run_token,
                        cursor_id,
                        processed_count,
                        total_count,
                    )
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?
                    .is_none()
            {
                break;
            }
            if item_ids.len() < PERSON_INDEX_REBUILD_BATCH_SIZE as usize {
                break;
            }
        }
        Ok(processed_count.saturating_sub(initial_processed_count) as usize)
    }

    pub(super) async fn restore_legacy_person_directories(
        &self,
        database: &Database,
    ) -> Result<usize, PeopleError> {
        let people_root = metadata_root(&self.config_dir).join(LEGACY_PEOPLE_DIR);
        let roots = [people_root.clone(), people_root.join("person")];
        let mut restored = 0;
        for root in roots {
            restored += self.restore_legacy_person_root(&root, database).await?;
        }
        Ok(restored)
    }

    pub(super) async fn restore_legacy_person_root(
        &self,
        root: &Path,
        database: &Database,
    ) -> Result<usize, PeopleError> {
        let mut buckets = match fs::read_dir(root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(source) => {
                return Err(PeopleError::Io {
                    path: root.to_owned(),
                    source,
                });
            }
        };
        let reserved = ["person", "index", "assets", "items", "profiles", "matches"];
        let mut restored = 0;
        while let Some(bucket) = buckets
            .next_entry()
            .await
            .map_err(|source| PeopleError::Io {
                path: root.to_owned(),
                source,
            })?
        {
            let bucket_path = bucket.path();
            let bucket_name = bucket.file_name();
            if reserved
                .iter()
                .any(|name| bucket_name.to_string_lossy().eq_ignore_ascii_case(name))
                || safe_metadata(&bucket_path)
                    .await?
                    .is_none_or(|metadata| !metadata.is_dir())
            {
                continue;
            }
            let mut persons =
                fs::read_dir(&bucket_path)
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: bucket_path.clone(),
                        source,
                    })?;
            while let Some(person_entry) =
                persons
                    .next_entry()
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: bucket_path.clone(),
                        source,
                    })?
            {
                let source_dir = person_entry.path();
                if safe_metadata(&source_dir)
                    .await?
                    .is_none_or(|metadata| !metadata.is_dir())
                {
                    continue;
                }
                let manifest_path = source_dir.join(PERSON_MANIFEST);
                if safe_metadata(&manifest_path).await?.is_some() {
                    continue;
                }
                let nfo_path = source_dir.join(PERSON_NFO);
                let Some(nfo_bytes) = read_people_file(&nfo_path).await? else {
                    continue;
                };
                let Some(parsed) = parse_person_nfo(&nfo_bytes) else {
                    tracing::warn!(path = %nfo_path.display(), "skipping malformed legacy person NFO");
                    continue;
                };
                let identities = parsed
                    .uniqueids
                    .iter()
                    .filter(|(provider, provider_id)| {
                        is_valid_person_id(provider) && is_valid_person_id(provider_id)
                    })
                    .map(|(provider, provider_id)| PersonIdentity {
                        provider: provider.clone(),
                        id: provider_id.clone(),
                    })
                    .collect::<Vec<_>>();
                let Some(primary) = identities.first() else {
                    continue;
                };
                let Some(display_name) = parsed
                    .fields
                    .get("name")
                    .map(String::as_str)
                    .filter(|name| !name.trim().is_empty())
                else {
                    tracing::warn!(path = %nfo_path.display(), "skipping legacy person NFO without a name");
                    continue;
                };
                let person = match database
                    .resolve_or_create_canonical_person(
                        display_name,
                        &primary.provider,
                        &primary.id,
                        "RECOVERED_LEGACY_NFO",
                        Some(1.0),
                        r#"{"method":"legacy-person-nfo"}"#,
                    )
                    .await
                {
                    Ok(person) => person,
                    Err(error) => {
                        tracing::warn!(path = %nfo_path.display(), %error, "skipping legacy person with conflicting identity");
                        continue;
                    }
                };
                let mut identities_attached = true;
                for identity in identities.iter().skip(1) {
                    if let Err(error) = database
                        .attach_canonical_person_identity(
                            &person.id,
                            &identity.provider,
                            &identity.id,
                            "RECOVERED_LEGACY_NFO",
                            Some(1.0),
                            r#"{"method":"legacy-person-nfo"}"#,
                        )
                        .await
                    {
                        tracing::warn!(path = %nfo_path.display(), %error, "skipping conflicting legacy person identity");
                        identities_attached = false;
                        break;
                    }
                }
                if !identities_attached {
                    continue;
                }
                let target_dir = match lux_person_directory(
                    &self.config_dir,
                    display_name,
                    &person.id,
                ) {
                    Ok(path) => path,
                    Err(error) => {
                        tracing::warn!(path = %nfo_path.display(), %error, "skipping legacy person with unsafe name");
                        continue;
                    }
                };
                if let Some(bytes) = read_people_file(&target_dir.join(PERSON_MANIFEST)).await?
                    && let Ok(manifest) = serde_json::from_slice::<PersonManifest>(&bytes)
                    && manifest.lux_person_id == person.id
                    && identities.iter().all(|identity| {
                        manifest
                            .identities
                            .iter()
                            .any(|existing| existing == identity)
                    })
                {
                    continue;
                }
                if let Err(error) = create_private_dir(&target_dir).await {
                    tracing::warn!(path = %nfo_path.display(), %error, "could not create migrated person directory");
                    continue;
                }
                if let Err(error) = self
                    .migrate_person_assets_from_directory(&source_dir, &target_dir)
                    .await
                {
                    tracing::warn!(path = %nfo_path.display(), %error, "could not migrate legacy person assets");
                    continue;
                }
                let actor = ActorCredit {
                    id: primary.id.clone(),
                    provider: Some(primary.provider.clone()),
                    identities: identities.clone(),
                    name: display_name.to_owned(),
                    character: None,
                    order: None,
                    profile_url: None,
                    person: None,
                };
                let _ = self
                    .persist_person_assets(
                        &actor,
                        &primary.provider,
                        &primary.id,
                        Some(&person.id),
                        &identities,
                    )
                    .await;
                restored += 1;
            }
        }
        Ok(restored)
    }

    pub(super) async fn restore_person_manifests(
        &self,
        database: &Database,
    ) -> Result<PersonManifestRestoreReport, PeopleError> {
        let root = metadata_root(&self.config_dir)
            .join(LEGACY_PEOPLE_DIR)
            .join("person");
        let mut initials = match fs::read_dir(&root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PersonManifestRestoreReport {
                    failed: true,
                    ..PersonManifestRestoreReport::default()
                });
            }
            Err(source) => {
                return Err(PeopleError::Io { path: root, source });
            }
        };
        let mut report = PersonManifestRestoreReport::default();
        while let Some(initial) = initials
            .next_entry()
            .await
            .map_err(|source| PeopleError::Io {
                path: root.clone(),
                source,
            })?
        {
            let initial_path = initial.path();
            if safe_metadata(&initial_path)
                .await?
                .is_none_or(|metadata| !metadata.is_dir())
            {
                continue;
            }
            let mut persons =
                fs::read_dir(&initial_path)
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: initial_path.clone(),
                        source,
                    })?;
            while let Some(person) =
                persons
                    .next_entry()
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: initial_path.clone(),
                        source,
                    })?
            {
                let person_dir = person.path();
                if safe_metadata(&person_dir)
                    .await?
                    .is_none_or(|metadata| !metadata.is_dir())
                {
                    continue;
                }
                let manifest_path = person_dir.join(PERSON_MANIFEST);
                let Some(bytes) = (match read_people_file(&manifest_path).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(path = %manifest_path.display(), %error, "skipping unreadable person manifest");
                        continue;
                    }
                }) else {
                    continue;
                };
                let manifest = match serde_json::from_slice::<PersonManifest>(&bytes) {
                    Ok(manifest) if valid_person_manifest(&manifest) => manifest,
                    Ok(_) | Err(_) => {
                        tracing::warn!(path = %manifest_path.display(), "skipping invalid person manifest");
                        continue;
                    }
                };
                let identities = manifest
                    .identities
                    .iter()
                    .map(|identity| (identity.provider.as_str(), identity.id.as_str()))
                    .collect::<Vec<_>>();
                match database
                    .restore_canonical_person_if_manifest_changed(
                        &manifest.lux_person_id,
                        &manifest.display_name,
                        &identities,
                        &manifest.checksum,
                        manifest.schema_version as i64,
                    )
                    .await
                {
                    Ok(true) => report.restored += 1,
                    Ok(false) => {}
                    Err(error) => {
                        report.failed = true;
                        tracing::warn!(
                            path = %manifest_path.display(),
                            %error,
                            "skipping conflicting person manifest"
                        );
                    }
                }
            }
        }
        Ok(report)
    }

    pub(super) async fn relation_has_matching_media_source(
        &self,
        database: &Database,
        relation: &StoredPeopleRelation,
    ) -> Result<bool, PeopleError> {
        let (Some(source_root), Some(source_relative_path)) = (
            relation.source_root.as_deref(),
            relation.source_relative_path.as_deref(),
        ) else {
            return Ok(false);
        };
        let source_locator = database
            .find_item_by_source_locator(source_root, source_relative_path)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        match source_locator {
            Some(locator) if relation_source_snapshot_matches(relation, &locator) => Ok(true),
            Some(_) | None => {
                let Some(fingerprint) = relation
                    .media_fingerprint
                    .as_deref()
                    .and_then(decode_fingerprint)
                else {
                    return Ok(false);
                };
                let mut candidates = database
                    .find_items_by_source_fingerprint(&fingerprint)
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?;
                candidates.retain(|candidate| relation_media_snapshot_matches(relation, candidate));
                Ok(candidates.len() == 1)
            }
        }
    }

    pub(super) async fn quarantine_person_relation_snapshot(
        &self,
        relation_path: &Path,
    ) -> Result<bool, PeopleError> {
        if safe_metadata(relation_path)
            .await?
            .is_none_or(|metadata| !metadata.is_file())
        {
            return Ok(false);
        }
        let quarantine_root = metadata_root(&self.config_dir)
            .join("quarantine")
            .join(PEOPLE_RELATION_QUARANTINE_DIR);
        create_private_dir(&quarantine_root).await?;
        let target = quarantine_root.join(format!("relation-{}.json", Uuid::now_v7()));
        fs::rename(relation_path, &target)
            .await
            .map_err(|source| PeopleError::Io {
                path: target.clone(),
                source,
            })?;
        restrict_permissions(&target, false).await?;
        Ok(true)
    }

    fn person_credits_from_relation_with_lookup(
        &self,
        relation: &StoredPeopleRelation,
        lookup: &BTreeMap<(String, String), String>,
    ) -> Vec<NewPersonCredit> {
        let mut credits = Vec::new();
        for actor in relation.actors.iter().take(MAX_ACTORS) {
            if actor.name.trim().is_empty() {
                continue;
            }
            let mut actor = actor.clone();
            if !actor.identities.is_empty() {
                let mut mapped_person_id = None;
                let mut conflicting = false;
                for identity in &actor.identities {
                    let Some(mapped) =
                        lookup.get(&(identity.provider.clone(), identity.id.clone()))
                    else {
                        continue;
                    };
                    if mapped_person_id
                        .as_ref()
                        .is_some_and(|person_id| person_id != mapped)
                    {
                        conflicting = true;
                        break;
                    }
                    mapped_person_id = Some(mapped.clone());
                }
                actor.person_key = (!conflicting).then_some(mapped_person_id.clone()).flatten();
                actor.lux_person_id = actor.person_key.clone();
            }
            credits.push(person_credit_from_stored_actor(&actor));
        }
        credits
    }

    async fn read_person_relation_with_path(
        &self,
        item_id: &str,
    ) -> Result<Option<(PathBuf, StoredPeopleRelation)>, PeopleError> {
        let new_path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let legacy_path = self
            .legacy_people_dir()
            .join(LEGACY_ITEMS_DIR)
            .join(format!("{item_id}.json"));
        match read_relation(&new_path).await? {
            Some(relation) => Ok(Some((new_path, relation))),
            None => Ok(read_relation(&legacy_path)
                .await?
                .map(|relation| (legacy_path, relation))),
        }
    }
}
