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
        acquire_person_manifest_lock(&manifest_path).await?;
        let result = async {
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
            Ok::<_, PeopleError>(locked_fields.iter().cloned().collect::<Vec<_>>())
        }
        .await;
        let _ = fs::remove_file(&manifest_path.with_file_name(".person.json.lock")).await;
        result
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
        acquire_person_manifest_lock(&manifest_path).await?;
        let result = async {
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
            write_atomically(&manifest_path, &bytes).await
        }
        .await;
        let _ = fs::remove_file(&manifest_path.with_file_name(".person.json.lock")).await;
        result
    }
}
