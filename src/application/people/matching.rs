use super::*;

impl PeopleService {
    pub async fn list_library_actors(
        &self,
        library_id: &str,
        person_type: &str,
        options: PersonListOptions,
    ) -> Result<(Vec<ActorView>, i64), PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let (credits, total) = database
            .list_person_credits_for_library(library_id, person_type, options)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        Ok((self.actor_views_from_credits(credits).await, total))
    }

    pub async fn list_pending_person_match_candidates(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<PersonMatchCandidateView>, i64), PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let total = database
            .count_pending_person_match_candidates()
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let candidates = database
            .list_pending_person_match_candidates(offset, limit)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?
            .into_iter()
            .map(person_match_candidate_view)
            .collect();
        Ok((candidates, total))
    }

    pub async fn set_person_field_locks(
        &self,
        person_id: &str,
        fields: &[String],
        evidence_json: &str,
    ) -> Result<Vec<String>, PeopleError> {
        if !is_valid_person_id(person_id) {
            return Err(PeopleError::InvalidComponent(person_id.to_owned()));
        }
        let mut locked_fields = BTreeSet::new();
        for field in fields {
            if !PERSON_LOCKABLE_FIELDS.contains(&field.as_str()) {
                return Err(PeopleError::InvalidComponent(field.clone()));
            }
            locked_fields.insert(field.clone());
        }
        let manifest_path = self.find_person_manifest_path(person_id, person_id).await?;
        let parent = manifest_path.parent().ok_or_else(|| {
            PeopleError::Serialization("person manifest path has no parent".to_owned())
        })?;
        create_private_dir(parent).await?;
        let _manifest_lock_guard = acquire_person_manifest_lock(&manifest_path).await?;
        async {
            let existing = read_people_file(&manifest_path).await?;
            let mut manifest = existing
                .map(|bytes| {
                    serde_json::from_slice::<PersonManifest>(&bytes)
                        .map_err(|source| PeopleError::Serialization(source.to_string()))
                })
                .transpose()?
                .unwrap_or_default();
            if !manifest.lux_person_id.is_empty() && manifest.lux_person_id != person_id {
                return Err(PeopleError::Serialization(
                    "person manifest identity does not match directory".to_owned(),
                ));
            }
            manifest.schema_version = PERSON_MANIFEST_SCHEMA_VERSION;
            manifest.generation = manifest.generation.saturating_add(1).max(1);
            manifest.lux_person_id = person_id.to_owned();
            manifest.locked_fields = locked_fields.clone();
            let event = PersonManifestMetadataEvent {
                event_id: Uuid::now_v7().to_string(),
                event_type: "FIELD_LOCKS_UPDATED".to_owned(),
                fields: locked_fields.iter().cloned().collect(),
                evidence_json: evidence_json.to_owned(),
                created_at: current_people_unix_timestamp(),
            };
            manifest.metadata_events.push(event);
            manifest.checksum.clear();
            let checksum_source = serde_json::to_vec(&manifest)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            manifest.checksum = Sha256::digest(checksum_source)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let bytes = serde_json::to_vec_pretty(&manifest)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            self.mark_person_manifest_restore_pending().await?;
            write_atomically(&manifest_path, &bytes).await?;
            self.record_person_manifest_path(person_id, &manifest_path)
                .await;
            Ok::<_, PeopleError>(locked_fields.iter().cloned().collect::<Vec<_>>())
        }
        .await
    }

    pub(super) async fn persist_person_match_candidate_snapshot(
        &self,
        mut snapshot: PersonMatchCandidateSnapshot,
    ) -> Result<(), PeopleError> {
        if !is_valid_person_id(&snapshot.id) {
            return Err(PeopleError::InvalidComponent(snapshot.id));
        }
        let directory = metadata_root(&self.config_dir)
            .join(LEGACY_PEOPLE_DIR)
            .join(PERSON_MATCH_SNAPSHOT_DIR);
        create_private_dir(&directory).await?;
        snapshot.schema_version = PERSON_MATCH_SNAPSHOT_SCHEMA_VERSION;
        snapshot.checksum.clear();
        let checksum_source = serde_json::to_vec(&snapshot)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        snapshot.checksum = Sha256::digest(checksum_source)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let bytes = serde_json::to_vec_pretty(&snapshot)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        write_atomically(&directory.join(format!("{}.json", snapshot.id)), &bytes).await
    }

    pub(super) async fn persist_person_decision_operation(
        &self,
        mut operation: PersonDecisionOperation,
    ) -> Result<(), PeopleError> {
        if !is_valid_person_id(&operation.operation_id)
            || !is_valid_person_id(&operation.candidate_id)
            || !is_valid_person_id(&operation.provider)
            || !is_valid_person_id(&operation.provider_id)
            || !is_valid_person_id(&operation.target_person_id)
            || operation
                .previous_person_id
                .as_deref()
                .is_some_and(|id| !is_valid_person_id(id))
        {
            return Err(PeopleError::InvalidComponent(
                operation.operation_id.clone(),
            ));
        }
        let directory = metadata_root(&self.config_dir)
            .join(LEGACY_PEOPLE_DIR)
            .join(PERSON_DECISION_OPERATION_DIR);
        create_private_dir(&directory).await?;
        operation.schema_version = PERSON_DECISION_OPERATION_SCHEMA_VERSION;
        operation.checksum.clear();
        let checksum_source = serde_json::to_vec(&operation)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        operation.checksum = Sha256::digest(checksum_source)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let bytes = serde_json::to_vec_pretty(&operation)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        write_atomically(
            &directory.join(format!("{}.json", operation.operation_id)),
            &bytes,
        )
        .await
    }

    pub(super) async fn replay_person_decision_operations(
        &self,
        database: &Database,
    ) -> Result<usize, PeopleError> {
        let directory = metadata_root(&self.config_dir)
            .join(LEGACY_PEOPLE_DIR)
            .join(PERSON_DECISION_OPERATION_DIR);
        let mut entries = match fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(source) => {
                return Err(PeopleError::Io {
                    path: directory,
                    source,
                });
            }
        };
        let mut replayed = 0;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| PeopleError::Io {
                path: directory.clone(),
                source,
            })?
        {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(bytes) = read_people_file(&path).await? else {
                continue;
            };
            let operation = match serde_json::from_slice::<PersonDecisionOperation>(&bytes) {
                Ok(operation) if valid_person_decision_operation(&operation) => operation,
                Ok(_) | Err(_) => {
                    tracing::warn!(path = %path.display(), "skipping invalid person decision operation");
                    continue;
                }
            };
            if operation.state == "COMPLETED" {
                continue;
            }
            let candidate = database
                .find_person_match_candidate(&operation.candidate_id)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
            let candidate = match candidate {
                Some(candidate) => candidate,
                None if operation.state == "COMMITTED" => {
                    let status = if operation.operation == "UNDO" {
                        "REJECTED"
                    } else {
                        "CONFIRMED"
                    };
                    let restore = PersonMatchCandidateRestore {
                        candidate_id: &operation.candidate_id,
                        item_id: &operation.item_id,
                        provider: &operation.provider,
                        provider_id: &operation.provider_id,
                        candidate_person_ids_json: &operation.candidate_person_ids_json,
                        status,
                        score: operation.score,
                        evidence_json: &operation.evidence_json,
                        target_person_id: Some(&operation.target_person_id),
                        previous_person_id: operation.previous_person_id.as_deref(),
                        created_at: operation.created_at,
                        updated_at: operation.updated_at,
                    };
                    database
                        .restore_person_match_candidate(&restore)
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?;
                    database
                        .find_person_match_candidate(&operation.candidate_id)
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?
                        .ok_or_else(|| {
                            PeopleError::Storage(
                                "restored person decision candidate could not be read".to_owned(),
                            )
                        })?
                }
                None => continue,
            };
            let committed = match operation.operation.as_str() {
                "CONFIRM" => candidate.status == "CONFIRMED",
                "UNDO" => candidate.status == "REJECTED",
                _ => false,
            };
            if !committed {
                continue;
            }
            let event = PersonManifestIdentityEvent {
                event_id: operation.operation_id.clone(),
                event_type: if operation.operation == "UNDO" {
                    "MANUAL_UNDO".to_owned()
                } else {
                    "MANUAL_CONFIRM".to_owned()
                },
                provider: operation.provider.clone(),
                provider_id: operation.provider_id.clone(),
                from_person_id: if operation.operation == "UNDO" {
                    Some(operation.target_person_id.clone())
                } else {
                    operation.previous_person_id.clone()
                },
                to_person_id: if operation.operation == "UNDO" {
                    operation.previous_person_id.clone()
                } else {
                    Some(operation.target_person_id.clone())
                },
                evidence_json: operation.evidence_json.clone(),
                created_at: operation.created_at,
            };
            if operation.operation == "UNDO" {
                self.update_person_manifest_identity(
                    &operation.target_person_id,
                    None,
                    None,
                    Some((&operation.provider, &operation.provider_id)),
                    &event,
                )
                .await?;
                if let Some(previous) = operation.previous_person_id.as_deref() {
                    let name = database
                        .find_canonical_person_display_name(previous)
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?
                        .unwrap_or_else(|| previous.to_owned());
                    self.update_person_manifest_identity(
                        previous,
                        Some(&name),
                        Some((&operation.provider, &operation.provider_id)),
                        None,
                        &event,
                    )
                    .await?;
                }
            } else {
                if let Some(previous) = operation.previous_person_id.as_deref()
                    && previous != operation.target_person_id
                {
                    self.update_person_manifest_identity(
                        previous,
                        None,
                        None,
                        Some((&operation.provider, &operation.provider_id)),
                        &event,
                    )
                    .await?;
                }
                let name = database
                    .find_canonical_person_display_name(&operation.target_person_id)
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?
                    .unwrap_or_else(|| operation.target_person_id.clone());
                self.update_person_manifest_identity(
                    &operation.target_person_id,
                    Some(&name),
                    Some((&operation.provider, &operation.provider_id)),
                    None,
                    &event,
                )
                .await?;
            }
            let mut completed = operation;
            completed.state = "COMPLETED".to_owned();
            completed.updated_at = current_people_unix_timestamp();
            self.persist_person_decision_operation(completed).await?;
            replayed += 1;
        }
        Ok(replayed)
    }

    pub(super) async fn restore_person_match_candidate_snapshots(
        &self,
        database: &Database,
    ) -> Result<usize, PeopleError> {
        let directory = metadata_root(&self.config_dir)
            .join(LEGACY_PEOPLE_DIR)
            .join(PERSON_MATCH_SNAPSHOT_DIR);
        let mut entries = match fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(source) => {
                return Err(PeopleError::Io {
                    path: directory,
                    source,
                });
            }
        };
        let mut restored = 0;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| PeopleError::Io {
                path: directory.clone(),
                source,
            })?
        {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(bytes) = read_people_file(&path).await? else {
                continue;
            };
            let snapshot = match serde_json::from_slice::<PersonMatchCandidateSnapshot>(&bytes) {
                Ok(snapshot) if valid_person_match_snapshot(&snapshot) => snapshot,
                Ok(_) | Err(_) => {
                    tracing::warn!(path = %path.display(), "skipping invalid person match snapshot");
                    continue;
                }
            };
            let candidate_person_ids = serde_json::to_string(&snapshot.candidate_person_ids)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            let evidence = snapshot.evidence.to_string();
            let restore = PersonMatchCandidateRestore {
                candidate_id: &snapshot.id,
                item_id: &snapshot.item_id,
                provider: &snapshot.provider,
                provider_id: &snapshot.provider_id,
                candidate_person_ids_json: &candidate_person_ids,
                status: &snapshot.status,
                score: snapshot.score,
                evidence_json: &evidence,
                target_person_id: snapshot.target_person_id.as_deref(),
                previous_person_id: snapshot.previous_person_id.as_deref(),
                created_at: snapshot.created_at,
                updated_at: snapshot.updated_at,
            };
            match database.restore_person_match_candidate(&restore).await {
                Ok(_) => restored += 1,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "skipping person match snapshot during restore")
                }
            }
        }
        Ok(restored)
    }

    pub async fn confirm_person_match_candidate(
        &self,
        candidate_id: &str,
        target_person_id: &str,
        evidence_json: &str,
    ) -> Result<PersonIdentityMove, PeopleError> {
        if !is_valid_person_id(candidate_id) || !is_valid_person_id(target_person_id) {
            return Err(PeopleError::InvalidComponent(candidate_id.to_owned()));
        }
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let candidate = database
            .find_person_match_candidate(candidate_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?
            .ok_or_else(|| {
                PeopleError::Storage(format!("person match candidate '{candidate_id}' not found"))
            })?;
        if candidate.status != "PENDING" {
            return Err(PeopleError::Storage(format!(
                "person match candidate '{candidate_id}' is {}",
                candidate.status
            )));
        }
        let previous_person_id = database
            .find_canonical_person_by_identity(&candidate.provider, &candidate.provider_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?
            .map(|person| person.id);
        let operation_id = Uuid::now_v7().to_string();
        let operation = PersonDecisionOperation {
            schema_version: PERSON_DECISION_OPERATION_SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            operation: "CONFIRM".to_owned(),
            candidate_id: candidate.id.clone(),
            item_id: candidate.item_id.clone(),
            candidate_person_ids_json: candidate.candidate_person_ids_json.clone(),
            score: candidate.score,
            provider: candidate.provider.clone(),
            provider_id: candidate.provider_id.clone(),
            target_person_id: target_person_id.to_owned(),
            previous_person_id: previous_person_id.clone(),
            state: "PREPARED".to_owned(),
            evidence_json: evidence_json.to_owned(),
            created_at: current_people_unix_timestamp(),
            updated_at: current_people_unix_timestamp(),
            checksum: String::new(),
        };
        self.persist_person_decision_operation(operation.clone())
            .await?;
        let movement = database
            .confirm_person_match_candidate(candidate_id, target_person_id, evidence_json)
            .await
            .map(|movement| PersonIdentityMove {
                previous_person_id: movement.previous_person_id,
            })
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let mut committed_operation = operation;
        committed_operation.state = "COMMITTED".to_owned();
        committed_operation.previous_person_id = movement.previous_person_id.clone();
        committed_operation.updated_at = current_people_unix_timestamp();
        self.persist_person_decision_operation(committed_operation.clone())
            .await?;
        if movement.previous_person_id.as_deref() != Some(target_person_id) {
            let target_name = database
                .find_canonical_person_display_name(target_person_id)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
                .unwrap_or_else(|| target_person_id.to_owned());
            let event_id = Uuid::now_v7().to_string();
            let event = PersonManifestIdentityEvent {
                event_id,
                event_type: "MANUAL_CONFIRM".to_owned(),
                provider: candidate.provider.clone(),
                provider_id: candidate.provider_id.clone(),
                from_person_id: movement.previous_person_id.clone(),
                to_person_id: Some(target_person_id.to_owned()),
                evidence_json: evidence_json.to_owned(),
                created_at: current_people_unix_timestamp(),
            };
            if let Some(previous_person_id) = movement.previous_person_id.as_deref() {
                self.update_person_manifest_identity(
                    previous_person_id,
                    None,
                    None,
                    Some((&candidate.provider, &candidate.provider_id)),
                    &event,
                )
                .await?;
            }
            self.update_person_manifest_identity(
                target_person_id,
                Some(&target_name),
                Some((&candidate.provider, &candidate.provider_id)),
                None,
                &event,
            )
            .await?;
        }
        let candidate_person_ids =
            serde_json::from_str(&candidate.candidate_person_ids_json).unwrap_or_default();
        self.persist_person_match_candidate_snapshot(PersonMatchCandidateSnapshot {
            schema_version: PERSON_MATCH_SNAPSHOT_SCHEMA_VERSION,
            id: candidate.id,
            item_id: candidate.item_id,
            provider: candidate.provider,
            provider_id: candidate.provider_id,
            candidate_person_ids,
            status: "CONFIRMED".to_owned(),
            score: candidate.score,
            evidence: serde_json::from_str(evidence_json)
                .unwrap_or(Value::String(evidence_json.to_owned())),
            target_person_id: Some(target_person_id.to_owned()),
            previous_person_id: movement.previous_person_id.clone(),
            created_at: candidate.created_at,
            updated_at: current_people_unix_timestamp(),
            checksum: String::new(),
        })
        .await?;
        committed_operation.state = "COMPLETED".to_owned();
        committed_operation.updated_at = current_people_unix_timestamp();
        self.persist_person_decision_operation(committed_operation)
            .await?;
        Ok(movement)
    }

    pub async fn reject_person_match_candidate(
        &self,
        candidate_id: &str,
        evidence_json: &str,
    ) -> Result<(), PeopleError> {
        if !is_valid_person_id(candidate_id) {
            return Err(PeopleError::InvalidComponent(candidate_id.to_owned()));
        }
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let candidate = database
            .find_person_match_candidate(candidate_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?
            .ok_or_else(|| {
                PeopleError::Storage(format!("person match candidate '{candidate_id}' not found"))
            })?;
        database
            .reject_person_match_candidate(candidate_id, evidence_json)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        self.persist_person_match_candidate_snapshot(PersonMatchCandidateSnapshot {
            schema_version: PERSON_MATCH_SNAPSHOT_SCHEMA_VERSION,
            id: candidate.id,
            item_id: candidate.item_id,
            provider: candidate.provider,
            provider_id: candidate.provider_id,
            candidate_person_ids: serde_json::from_str(&candidate.candidate_person_ids_json)
                .unwrap_or_default(),
            status: "REJECTED".to_owned(),
            score: candidate.score,
            evidence: serde_json::from_str(evidence_json)
                .unwrap_or(Value::String(evidence_json.to_owned())),
            target_person_id: candidate.target_person_id,
            previous_person_id: candidate.previous_person_id,
            created_at: candidate.created_at,
            updated_at: current_people_unix_timestamp(),
            checksum: String::new(),
        })
        .await
        .map_err(|error| PeopleError::Storage(error.to_string()))
    }

    pub async fn undo_person_match_candidate(
        &self,
        candidate_id: &str,
        evidence_json: &str,
    ) -> Result<PersonIdentityMove, PeopleError> {
        if !is_valid_person_id(candidate_id) {
            return Err(PeopleError::InvalidComponent(candidate_id.to_owned()));
        }
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let candidate = database
            .find_person_match_candidate(candidate_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?
            .ok_or_else(|| {
                PeopleError::Storage(format!("person match candidate '{candidate_id}' not found"))
            })?;
        if candidate.status != "CONFIRMED" {
            return Err(PeopleError::Storage(format!(
                "person match candidate '{candidate_id}' is {}",
                candidate.status
            )));
        }
        let Some(target_person_id) = candidate.target_person_id.clone() else {
            return Err(PeopleError::Storage(
                "confirmed person match has no recorded target identity".to_owned(),
            ));
        };
        let previous_person_id = candidate.previous_person_id.clone();
        let operation_id = Uuid::now_v7().to_string();
        let operation = PersonDecisionOperation {
            schema_version: PERSON_DECISION_OPERATION_SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            operation: "UNDO".to_owned(),
            candidate_id: candidate.id.clone(),
            item_id: candidate.item_id.clone(),
            candidate_person_ids_json: candidate.candidate_person_ids_json.clone(),
            score: candidate.score,
            provider: candidate.provider.clone(),
            provider_id: candidate.provider_id.clone(),
            target_person_id: target_person_id.clone(),
            previous_person_id: previous_person_id.clone(),
            state: "PREPARED".to_owned(),
            evidence_json: evidence_json.to_owned(),
            created_at: current_people_unix_timestamp(),
            updated_at: current_people_unix_timestamp(),
            checksum: String::new(),
        };
        self.persist_person_decision_operation(operation.clone())
            .await?;
        let movement = database
            .undo_person_match_candidate(candidate_id, evidence_json)
            .await
            .map(|movement| PersonIdentityMove {
                previous_person_id: movement.previous_person_id,
            })
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let mut committed_operation = operation;
        committed_operation.state = "COMMITTED".to_owned();
        committed_operation.updated_at = current_people_unix_timestamp();
        self.persist_person_decision_operation(committed_operation.clone())
            .await?;
        if previous_person_id.as_deref() != Some(target_person_id.as_str()) {
            let event = PersonManifestIdentityEvent {
                event_id: Uuid::now_v7().to_string(),
                event_type: "MANUAL_UNDO".to_owned(),
                provider: candidate.provider.clone(),
                provider_id: candidate.provider_id.clone(),
                from_person_id: Some(target_person_id.clone()),
                to_person_id: previous_person_id.clone(),
                evidence_json: evidence_json.to_owned(),
                created_at: current_people_unix_timestamp(),
            };
            self.update_person_manifest_identity(
                &target_person_id,
                None,
                None,
                Some((&candidate.provider, &candidate.provider_id)),
                &event,
            )
            .await?;
            if let Some(previous_person_id) = previous_person_id.as_deref() {
                let previous_name = database
                    .find_canonical_person_display_name(previous_person_id)
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?
                    .unwrap_or_else(|| previous_person_id.to_owned());
                self.update_person_manifest_identity(
                    previous_person_id,
                    Some(&previous_name),
                    Some((&candidate.provider, &candidate.provider_id)),
                    None,
                    &event,
                )
                .await?;
            }
        }
        let mut evidence = serde_json::from_str::<Value>(evidence_json)
            .unwrap_or(Value::String(evidence_json.to_owned()));
        if let Value::Object(object) = &mut evidence {
            object.insert("operation".to_owned(), Value::String("undo".to_owned()));
        }
        self.persist_person_match_candidate_snapshot(PersonMatchCandidateSnapshot {
            schema_version: PERSON_MATCH_SNAPSHOT_SCHEMA_VERSION,
            id: candidate.id,
            item_id: candidate.item_id,
            provider: candidate.provider,
            provider_id: candidate.provider_id,
            candidate_person_ids: serde_json::from_str(&candidate.candidate_person_ids_json)
                .unwrap_or_default(),
            status: "REJECTED".to_owned(),
            score: candidate.score,
            evidence,
            target_person_id: Some(target_person_id),
            previous_person_id,
            created_at: candidate.created_at,
            updated_at: current_people_unix_timestamp(),
            checksum: String::new(),
        })
        .await?;
        committed_operation.state = "COMPLETED".to_owned();
        committed_operation.updated_at = current_people_unix_timestamp();
        self.persist_person_decision_operation(committed_operation)
            .await?;
        Ok(movement)
    }

    pub async fn split_person_identity(
        &self,
        source_person_id: &str,
        provider: &str,
        provider_id: &str,
        display_name: &str,
        evidence_json: &str,
    ) -> Result<String, PeopleError> {
        if !is_valid_person_id(source_person_id)
            || !is_valid_person_id(provider)
            || !is_valid_person_id(provider_id)
            || !is_valid_person_lookup(display_name)
        {
            return Err(PeopleError::InvalidComponent(display_name.to_owned()));
        }
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let split = database
            .split_canonical_person_identity(
                source_person_id,
                provider,
                provider_id,
                display_name,
                evidence_json,
            )
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let event = PersonManifestIdentityEvent {
            event_id: Uuid::now_v7().to_string(),
            event_type: "MANUAL_SPLIT".to_owned(),
            provider: provider.to_owned(),
            provider_id: provider_id.to_owned(),
            from_person_id: Some(source_person_id.to_owned()),
            to_person_id: Some(split.id.clone()),
            evidence_json: evidence_json.to_owned(),
            created_at: current_people_unix_timestamp(),
        };
        self.update_person_manifest_identity(
            source_person_id,
            None,
            None,
            Some((provider, provider_id)),
            &event,
        )
        .await?;
        self.update_person_manifest_identity(
            &split.id,
            Some(display_name),
            Some((provider, provider_id)),
            None,
            &event,
        )
        .await?;
        Ok(split.id)
    }

    pub async fn merge_canonical_people(
        &self,
        source_person_id: &str,
        target_person_id: &str,
        evidence_json: &str,
    ) -> Result<CanonicalPersonMergeResult, PeopleError> {
        if !is_valid_person_id(source_person_id) {
            return Err(PeopleError::InvalidComponent(source_person_id.to_owned()));
        }
        if !is_valid_person_id(target_person_id) {
            return Err(PeopleError::InvalidComponent(target_person_id.to_owned()));
        }
        validate_component(source_person_id)?;
        validate_component(target_person_id)?;
        if source_person_id == target_person_id {
            return Err(PeopleError::Conflict(format!(
                "cannot merge canonical person '{source_person_id}' into itself"
            )));
        }

        let database = self
            .database
            .as_ref()
            .ok_or_else(|| PeopleError::Storage("people database is not configured".to_owned()))?;
        let merged = database
            .merge_canonical_person(
                source_person_id,
                target_person_id,
                "MANUAL_ADMIN_MERGE",
                Some(1.0),
                evidence_json,
            )
            .await
            .map_err(|error| match error {
                crate::storage::StorageError::Conflict(message)
                    if message.contains("not found") =>
                {
                    PeopleError::NotFound(message)
                }
                crate::storage::StorageError::Conflict(message) => PeopleError::Conflict(message),
                other => PeopleError::Storage(other.to_string()),
            })?;

        self.sync_merged_canonical_person_artifacts(&merged, "MANUAL_ADMIN_MERGE", evidence_json)
            .await?;

        Ok(CanonicalPersonMergeResult {
            source_person_id: merged.source_person_id,
            target_person_id: merged.target_person_id,
            moved_identities: merged
                .moved_identities
                .into_iter()
                .map(|(provider, id)| PersonIdentity { provider, id })
                .collect(),
        })
    }

    pub(super) async fn sync_merged_canonical_person_artifacts(
        &self,
        merged: &crate::storage::StoredCanonicalPersonMerge,
        event_type: &str,
        evidence_json: &str,
    ) -> Result<(), PeopleError> {
        let target_manifest_path = self
            .find_person_manifest_path(&merged.target_person_id, &merged.target_display_name)
            .await?;
        let source_manifest_path = self
            .find_person_manifest_path(&merged.source_person_id, &merged.source_display_name)
            .await?;
        let target_dir = target_manifest_path.parent().ok_or_else(|| {
            PeopleError::InvalidComponent("person manifest missing parent".to_owned())
        })?;
        let source_dir = source_manifest_path.parent().ok_or_else(|| {
            PeopleError::InvalidComponent("person manifest missing parent".to_owned())
        })?;

        let (_first_lock, _second_lock) = if source_manifest_path <= target_manifest_path {
            (
                acquire_person_manifest_lock(&source_manifest_path).await?,
                acquire_person_manifest_lock(&target_manifest_path).await?,
            )
        } else {
            (
                acquire_person_manifest_lock(&target_manifest_path).await?,
                acquire_person_manifest_lock(&source_manifest_path).await?,
            )
        };

        create_private_dir(target_dir).await?;
        create_private_dir(source_dir).await?;

        let source_manifest = match read_people_file(&source_manifest_path).await? {
            Some(bytes) => Some(
                serde_json::from_slice::<PersonManifest>(&bytes)
                    .map_err(|source| PeopleError::Serialization(source.to_string()))?,
            ),
            None => None,
        };
        let mut target_manifest = match read_people_file(&target_manifest_path).await? {
            Some(bytes) => serde_json::from_slice::<PersonManifest>(&bytes)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?,
            None => PersonManifest {
                schema_version: PERSON_MANIFEST_SCHEMA_VERSION,
                generation: 0,
                lux_person_id: merged.target_person_id.clone(),
                display_name: merged.target_display_name.clone(),
                ..PersonManifest::default()
            },
        };

        let mut target_image_path = find_existing_profile_image_path(target_dir).await?;
        if target_image_path.is_none() {
            let mut candidate_source_image = find_existing_profile_image_path(source_dir).await?;
            if candidate_source_image.is_none() {
                if let Some(image) = self
                    .indexed_profile_image_for_provider("lux", &merged.source_person_id)
                    .await?
                {
                    candidate_source_image = Some(image.path);
                }
            }
            if candidate_source_image.is_none() {
                for (provider, provider_id) in &merged.moved_identities {
                    if let Some(image) = self
                        .indexed_profile_image_for_provider(provider, provider_id)
                        .await?
                    {
                        candidate_source_image = Some(image.path);
                        break;
                    }
                }
            }
            if let Some(source_image) = candidate_source_image
                && let Some(ext) = source_image
                    .extension()
                    .and_then(|value| value.to_str())
                    .filter(|value| PROFILE_EXTENSIONS.contains(value))
            {
                let bytes = fs::read(&source_image)
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: source_image.clone(),
                        source,
                    })?;
                let destination = target_dir.join(format!("{PERSON_IMAGE}.{ext}"));
                write_atomically_if_changed(&destination, &bytes).await?;
                restrict_permissions(&destination, false).await?;
                target_image_path = Some(destination);
            }
        }

        target_manifest.merged_into_person_id = None;
        if target_manifest.display_name.trim().is_empty() {
            target_manifest.display_name = merged.target_display_name.clone();
        }
        if !merged.source_display_name.trim().is_empty()
            && merged.source_display_name != target_manifest.display_name
        {
            target_manifest
                .aliases
                .insert(merged.source_display_name.clone());
        }
        if let Some(source) = &source_manifest {
            for alias in &source.aliases {
                if !alias.trim().is_empty() && alias != &target_manifest.display_name {
                    target_manifest.aliases.insert(alias.clone());
                }
            }
            if let Some(source_person) = source.person.clone() {
                target_manifest.person = Some(match target_manifest.person.take() {
                    Some(existing) => existing.supplement_missing_from(source_person),
                    None => source_person,
                });
            }
        }

        let mut seen_identities = target_manifest
            .identities
            .iter()
            .map(|identity| (identity.provider.clone(), identity.id.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        for (provider, provider_id) in &merged.moved_identities {
            if seen_identities.insert((provider.clone(), provider_id.clone())) {
                target_manifest.identities.push(PersonIdentity {
                    provider: provider.clone(),
                    id: provider_id.clone(),
                });
            }
        }
        if let Some(source) = &source_manifest {
            for identity in &source.identities {
                if seen_identities.insert((identity.provider.clone(), identity.id.clone())) {
                    target_manifest.identities.push(identity.clone());
                }
            }
        }

        let source_nfo_bytes = read_people_file(&source_dir.join(PERSON_NFO)).await?;
        let target_nfo_path = target_dir.join(PERSON_NFO);
        let target_nfo_bytes = read_people_file(&target_nfo_path).await?;
        if target_nfo_bytes.is_some()
            || source_nfo_bytes.is_some()
            || target_manifest.person.is_some()
        {
            let (nfo_provider, nfo_id) = target_manifest
                .identities
                .first()
                .map(|identity| (identity.provider.as_str(), identity.id.as_str()))
                .unwrap_or(("lux", merged.target_person_id.as_str()));
            let base_nfo = target_nfo_bytes.as_deref().or(source_nfo_bytes.as_deref());
            let merged_nfo = match base_nfo {
                Some(existing) => merge_person_nfo_bytes(
                    existing,
                    &target_manifest.display_name,
                    nfo_provider,
                    nfo_id,
                    target_manifest.person.as_ref(),
                ),
                None => Some(person_nfo_bytes(
                    &target_manifest.display_name,
                    nfo_provider,
                    nfo_id,
                    target_manifest.person.as_ref(),
                )),
            };
            if let Some(merged_nfo) = merged_nfo {
                write_atomically_if_changed(&target_nfo_path, &merged_nfo).await?;
                restrict_permissions(&target_nfo_path, false).await?;
            }
        }

        let now = current_people_unix_timestamp();
        let (event_provider, event_provider_id) = merged
            .moved_identities
            .first()
            .map(|(provider, id)| (provider.clone(), id.clone()))
            .unwrap_or_else(|| ("lux".to_owned(), merged.source_person_id.clone()));
        let event = PersonManifestIdentityEvent {
            event_id: Uuid::now_v7().to_string(),
            event_type: event_type.to_owned(),
            provider: event_provider,
            provider_id: event_provider_id,
            from_person_id: Some(merged.source_person_id.clone()),
            to_person_id: Some(merged.target_person_id.clone()),
            evidence_json: evidence_json.to_owned(),
            created_at: now,
        };
        target_manifest.identity_events.push(event.clone());
        sign_and_write_person_manifest(&target_manifest_path, &mut target_manifest).await?;
        self.record_person_manifest_path(&merged.target_person_id, &target_manifest_path)
            .await;

        let mut updated_source_manifest = source_manifest.unwrap_or_else(|| PersonManifest {
            schema_version: PERSON_MANIFEST_SCHEMA_VERSION,
            generation: 0,
            lux_person_id: merged.source_person_id.clone(),
            display_name: merged.source_display_name.clone(),
            ..PersonManifest::default()
        });
        updated_source_manifest.identities.clear();
        updated_source_manifest.merged_into_person_id = Some(merged.target_person_id.clone());
        updated_source_manifest.identity_events.push(event);
        sign_and_write_person_manifest(&source_manifest_path, &mut updated_source_manifest).await?;
        self.record_person_manifest_path(&merged.source_person_id, &source_manifest_path)
            .await;

        if let Some(image_path) = target_image_path {
            let metadata_dir = metadata_root(&self.config_dir);
            let relative = metadata_relative_string(&metadata_dir, &image_path)?;
            let index = StoredPersonIndex {
                image_path: relative,
                person_key: Some(merged.target_person_id.clone()),
            };
            let index_bytes = serde_json::to_vec(&index)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            let mut index_paths = vec![
                people_index_path(&self.config_dir, &merged.target_person_id)?,
                people_index_path(&self.config_dir, &merged.source_person_id)?,
            ];
            for identity in &target_manifest.identities {
                if !is_valid_person_id(&identity.provider) || !is_valid_person_id(&identity.id) {
                    continue;
                }
                index_paths.push(people_index_path_for_provider(
                    &self.config_dir,
                    &identity.provider,
                    &identity.id,
                )?);
                if identity.provider.eq_ignore_ascii_case("tmdb") {
                    index_paths.push(people_index_path(&self.config_dir, &identity.id)?);
                }
            }
            for index_path in index_paths {
                if let Some(index_dir) = index_path.parent() {
                    create_private_dir(index_dir).await?;
                }
                write_atomically_if_changed(&index_path, &index_bytes).await?;
                restrict_permissions(&index_path, false).await?;
            }
        }

        Ok(())
    }

    pub(super) async fn update_person_manifest_identity(
        &self,
        person_id: &str,
        display_name: Option<&str>,
        add_identity: Option<(&str, &str)>,
        remove_identity: Option<(&str, &str)>,
        event: &PersonManifestIdentityEvent,
    ) -> Result<(), PeopleError> {
        let fallback_name = display_name.unwrap_or(person_id);
        let manifest_path = self
            .find_person_manifest_path(person_id, fallback_name)
            .await?;
        let parent = manifest_path.parent().ok_or_else(|| {
            PeopleError::Serialization("person manifest path has no parent".to_owned())
        })?;
        create_private_dir(parent).await?;
        let _manifest_lock_guard = acquire_person_manifest_lock(&manifest_path).await?;
        async {
            let existing = read_people_file(&manifest_path).await?;
            let mut manifest = existing
                .map(|bytes| {
                    serde_json::from_slice::<PersonManifest>(&bytes)
                        .map_err(|source| PeopleError::Serialization(source.to_string()))
                })
                .transpose()?
                .unwrap_or_default();
            if !manifest.lux_person_id.is_empty() && manifest.lux_person_id != person_id {
                return Err(PeopleError::Serialization(
                    "person manifest identity does not match directory".to_owned(),
                ));
            }
            manifest.schema_version = PERSON_MANIFEST_SCHEMA_VERSION;
            manifest.generation = manifest.generation.saturating_add(1).max(1);
            manifest.lux_person_id = person_id.to_owned();
            if manifest.display_name.is_empty() {
                manifest.display_name = fallback_name.to_owned();
            }
            if let Some((provider, provider_id)) = remove_identity {
                manifest
                    .identities
                    .retain(|identity| identity.provider != provider || identity.id != provider_id);
            }
            if let Some((provider, provider_id)) = add_identity
                && !manifest
                    .identities
                    .iter()
                    .any(|identity| identity.provider == provider && identity.id == provider_id)
            {
                manifest.identities.push(PersonIdentity {
                    provider: provider.to_owned(),
                    id: provider_id.to_owned(),
                });
            }
            manifest.identities.sort_by(|left, right| {
                left.provider
                    .cmp(&right.provider)
                    .then(left.id.cmp(&right.id))
            });
            if !manifest
                .identity_events
                .iter()
                .any(|existing| existing.event_id == event.event_id)
            {
                manifest.identity_events.push(event.clone());
            }
            manifest.checksum.clear();
            let checksum_source = serde_json::to_vec(&manifest)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            let digest = Sha256::digest(checksum_source);
            manifest.checksum = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            let bytes = serde_json::to_vec_pretty(&manifest)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            self.mark_person_manifest_restore_pending().await?;
            write_atomically(&manifest_path, &bytes).await?;
            self.record_person_manifest_path(person_id, &manifest_path)
                .await;
            Ok(())
        }
        .await
    }
}

async fn find_existing_profile_image_path(
    person_dir: &Path,
) -> Result<Option<PathBuf>, PeopleError> {
    for extension in PROFILE_EXTENSIONS {
        let candidate = person_dir.join(format!("{PERSON_IMAGE}.{extension}"));
        if safe_metadata(&candidate)
            .await?
            .is_some_and(|metadata| metadata.is_file())
        {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

async fn sign_and_write_person_manifest(
    manifest_path: &Path,
    manifest: &mut PersonManifest,
) -> Result<(), PeopleError> {
    manifest.generation = manifest.generation.saturating_add(1);
    manifest.checksum.clear();
    let checksum_source = serde_json::to_vec(manifest)
        .map_err(|source| PeopleError::Serialization(source.to_string()))?;
    let digest = Sha256::digest(checksum_source);
    manifest.checksum = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|source| PeopleError::Serialization(source.to_string()))?;
    write_atomically_if_changed(manifest_path, &bytes).await?;
    restrict_permissions(manifest_path, false).await
}

fn metadata_relative_string(metadata_dir: &Path, path: &Path) -> Result<String, PeopleError> {
    let relative = path.strip_prefix(metadata_dir).map_err(|_| {
        PeopleError::Serialization("person image path is outside metadata".to_owned())
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => parts.push(segment.to_string_lossy().into_owned()),
            _ => {
                return Err(PeopleError::Serialization(
                    "person image index contains an unsafe path".to_owned(),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(PeopleError::Serialization(
            "person image index path is empty".to_owned(),
        ));
    }
    Ok(parts.join("/"))
}
