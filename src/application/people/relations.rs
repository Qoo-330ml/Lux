use super::*;

impl PeopleService {
    pub(super) async fn resolve_person_key(
        &self,
        actor: &ActorCredit,
        identities: &[PersonIdentity],
        bridge_person_key: Option<&str>,
    ) -> Result<Option<String>, PeopleError> {
        if identities.is_empty() {
            return Ok(bridge_person_key.map(str::to_owned));
        }
        let Some(database) = &self.database else {
            return Ok(person_key_for_identities(identities));
        };

        let mut mapped_person: Option<StoredCanonicalPerson> = None;
        for identity in identities {
            let candidate = database
                .find_canonical_person_by_identity(&identity.provider, &identity.id)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
            let Some(candidate) = candidate else {
                continue;
            };
            if mapped_person
                .as_ref()
                .is_some_and(|person| person.id != candidate.id)
            {
                tracing::warn!(
                    actor = %actor.name,
                    "actor provider identities map to different canonical people"
                );
                return Ok(None);
            }
            mapped_person = Some(candidate);
        }

        if mapped_person.is_none()
            && let Some(bridge_person_key) = bridge_person_key
        {
            if identities.is_empty() {
                return Ok(Some(bridge_person_key.to_owned()));
            }
            for identity in identities {
                database
                    .attach_canonical_person_identity(
                        bridge_person_key,
                        &identity.provider,
                        &identity.id,
                        "SAME_MEDIA_BRIDGE",
                        Some(0.97),
                        r#"{"method":"same-media-bridge"}"#,
                    )
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?;
            }
            return Ok(Some(bridge_person_key.to_owned()));
        }

        if mapped_person.is_none()
            && let Some(incoming_birthday) = actor
                .person
                .as_ref()
                .and_then(|person| person.birthday.as_deref())
                .filter(|value| !value.trim().is_empty())
            && birthday_parts(Some(incoming_birthday)).is_some_and(|(_, _, day)| day.is_some())
        {
            let candidates = database
                .list_canonical_people_by_normalized_name(&normalize_person_match_text(&actor.name))
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
            let compatible = candidates
                .iter()
                .filter(|candidate| {
                    candidate.birthdays.iter().any(|birthday| {
                        birthday_parts(Some(birthday)).is_some_and(|(_, _, day)| day.is_some())
                            && birthdays_compatible(Some(incoming_birthday), Some(birthday))
                    })
                })
                .collect::<Vec<_>>();
            if compatible.len() == 1 {
                let person_id = &compatible[0].id;
                for identity in identities {
                    database
                        .attach_canonical_person_identity(
                            person_id,
                            &identity.provider,
                            &identity.id,
                            "NAME_BIRTHDAY_UNIQUE",
                            Some(0.96),
                            &serde_json::json!({
                                "method": "unique-name-birthday",
                                "normalizedName": normalize_person_match_text(&actor.name),
                                "birthday": incoming_birthday,
                            })
                            .to_string(),
                        )
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?;
                }
                return Ok(Some(person_id.to_owned()));
            }
        }

        let person = match mapped_person {
            Some(person) => person,
            None => database
                .resolve_or_create_canonical_person(
                    &actor.name,
                    &identities[0].provider,
                    &identities[0].id,
                    "PROVIDER_ID",
                    Some(1.0),
                    r#"{"method":"provider-id"}"#,
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?,
        };
        for identity in identities {
            if database
                .find_canonical_person_by_identity(&identity.provider, &identity.id)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
                .is_some()
            {
                continue;
            }
            database
                .attach_canonical_person_identity(
                    &person.id,
                    &identity.provider,
                    &identity.id,
                    "SAME_SOURCE_ID_SET",
                    Some(0.99),
                    r#"{"method":"same-source-id-set"}"#,
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
        }
        Ok(Some(person.id))
    }

    pub async fn persist_item_actors(
        &self,
        item_id: &str,
        provider: &str,
        actors: &[ActorCredit],
    ) -> Result<usize, PeopleError> {
        self.persist_item_actors_with_source(item_id, provider, actors, None)
            .await
            .map(|report| report.stored_count)
    }

    pub async fn persist_nfo_item_actors(
        &self,
        item_id: &str,
        provider: &str,
        actors: &[ActorCredit],
        source_fingerprint: &[u8],
    ) -> Result<ActorPersistReport, PeopleError> {
        self.persist_item_actors_with_source(item_id, provider, actors, Some(source_fingerprint))
            .await
    }

    pub(crate) async fn update_item_actor_metadata(
        &self,
        item_id: &str,
        provider: &str,
        actors: &[ActorCredit],
    ) -> Result<usize, PeopleError> {
        let new_path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let legacy_path = self
            .legacy_people_dir()
            .join(LEGACY_ITEMS_DIR)
            .join(format!("{item_id}.json"));
        let relation_path = if read_relation(&new_path).await?.is_some() {
            new_path
        } else {
            legacy_path
        };
        let lock_path = relation_path.with_file_name(".people.json.lock");
        acquire_exclusive_file_lock(&lock_path).await?;
        let result = self
            .update_item_actor_metadata_locked(item_id, &relation_path, provider, actors)
            .await;
        let _ = fs::remove_file(&lock_path).await;
        result
    }

    pub(super) async fn update_item_actor_metadata_locked(
        &self,
        item_id: &str,
        relation_path: &Path,
        provider: &str,
        actors: &[ActorCredit],
    ) -> Result<usize, PeopleError> {
        let Some(mut relation) = read_relation(relation_path).await? else {
            return Ok(0);
        };
        let mut changed_indices = Vec::new();
        for (index, stored_actor) in relation.actors.iter_mut().enumerate() {
            let Some(enriched) = actors.iter().find(|actor| {
                !actor.id.trim().is_empty()
                    && actor.id.trim() == actor_id_from_stored_actor(stored_actor)
                    && actor_provider_matches(stored_actor, actor, provider)
            }) else {
                continue;
            };
            let Some(person) = enriched.person.clone() else {
                continue;
            };
            let merged = stored_actor
                .person
                .take()
                .unwrap_or_default()
                .supplement_missing_from(person);
            if stored_actor.person.as_ref() != Some(&merged) {
                stored_actor.person = Some(merged);
                changed_indices.push(index);
            }
        }
        if changed_indices.is_empty() {
            return Ok(0);
        }

        for index in &changed_indices {
            let actor = &relation.actors[*index];
            self.write_person_nfo_for_actor(actor).await?;
            if let Some(person_key) = actor
                .person_key
                .as_deref()
                .filter(|key| key.starts_with("lux-"))
            {
                let actor_provider = actor_provider_from_stored_actor(actor)
                    .unwrap_or_else(|| provider.trim().to_ascii_lowercase());
                self.persist_person_manifest(
                    &lux_person_directory(&self.config_dir, &actor.name, person_key)
                        .map_err(PeopleError::from)?,
                    person_key,
                    &actor.name,
                    &actor_provider,
                    &actor.identities,
                    actor.person.as_ref(),
                )
                .await?;
            }
        }
        let bytes = serde_json::to_vec_pretty(&relation)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        write_atomically(relation_path, &bytes).await?;
        if let Some(database) = &self.database {
            let credits = relation
                .actors
                .iter()
                .map(person_credit_from_stored_actor)
                .collect::<Vec<_>>();
            database
                .replace_person_credits_with_fingerprint(
                    item_id,
                    &credits,
                    relation.source_fingerprint.as_deref(),
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
        }
        Ok(changed_indices.len())
    }

    pub(super) async fn persist_item_actors_with_source(
        &self,
        item_id: &str,
        provider: &str,
        actors: &[ActorCredit],
        source_fingerprint: Option<&[u8]>,
    ) -> Result<ActorPersistReport, PeopleError> {
        let relation_path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let relation_dir = relation_path.parent().ok_or_else(|| {
            PeopleError::Serialization("people relation path has no parent".to_owned())
        })?;
        create_private_dir(relation_dir).await?;
        let lock_path = relation_path.with_file_name(".people.json.lock");
        acquire_exclusive_file_lock(&lock_path).await?;
        let result = self
            .persist_item_actors_with_source_locked(
                item_id,
                provider,
                actors,
                source_fingerprint,
                &relation_path,
            )
            .await;
        let _ = fs::remove_file(&lock_path).await;
        result
    }

    pub(super) async fn persist_item_actors_with_source_locked(
        &self,
        item_id: &str,
        provider: &str,
        actors: &[ActorCredit],
        source_fingerprint: Option<&[u8]>,
        relation_path: &Path,
    ) -> Result<ActorPersistReport, PeopleError> {
        let previous_relation = read_relation(relation_path).await?;
        let source_locator = if let Some(database) = &self.database {
            match database.find_item_source_locator(item_id).await {
                Ok(locator) => locator,
                Err(error) => {
                    tracing::warn!(item_id, %error, "person relation source locator was unavailable");
                    None
                }
            }
        } else {
            None
        };

        let mut stored = Vec::new();
        let mut pending_assets = Vec::new();
        let provider = provider.trim().to_ascii_lowercase();
        for actor in actors.iter().take(MAX_ACTORS) {
            if actor.name.trim().is_empty() {
                continue;
            }
            let identities = actor_identities(actor, &provider);
            let primary = identities.first();
            let actor_id = primary
                .map(|identity| identity.id.as_str())
                .unwrap_or_default();
            let actor_provider = primary
                .map(|identity| identity.provider.as_str())
                .unwrap_or_default();
            let bridge_candidates = same_media_bridge_candidates(previous_relation.as_ref(), actor);
            if bridge_candidates.len() > 1
                && !identities.is_empty()
                && let Some(database) = &self.database
            {
                let candidate_ids = bridge_candidates
                    .iter()
                    .filter_map(|candidate| candidate.person_key.as_deref())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let evidence = serde_json::json!({
                    "method": "same-media-ambiguous",
                    "normalizedName": normalize_person_match_text(&actor.name),
                    "candidateCount": candidate_ids.len(),
                    "hasRole": actor.character.as_deref().is_some_and(|value| !value.trim().is_empty()),
                    "hasOrder": actor.order.is_some(),
                    "hasBirthday": actor.person.as_ref().and_then(|person| person.birthday.as_deref()).is_some(),
                });
                let candidate_ids_json = serde_json::to_string(&candidate_ids)
                    .map_err(|error| PeopleError::Serialization(error.to_string()))?;
                match database
                    .enqueue_person_match_candidate(
                        item_id,
                        actor_provider,
                        actor_id,
                        &candidate_ids_json,
                        Some(0.55),
                        &evidence.to_string(),
                    )
                    .await
                {
                    Ok(candidate_id) => {
                        let persisted = database
                            .find_person_match_candidate(&candidate_id)
                            .await
                            .ok()
                            .flatten();
                        let snapshot = PersonMatchCandidateSnapshot {
                            schema_version: PERSON_MATCH_SNAPSHOT_SCHEMA_VERSION,
                            id: candidate_id,
                            item_id: item_id.to_owned(),
                            provider: actor_provider.to_owned(),
                            provider_id: actor_id.to_owned(),
                            candidate_person_ids: candidate_ids,
                            status: persisted
                                .as_ref()
                                .map(|candidate| candidate.status.clone())
                                .unwrap_or_else(|| "PENDING".to_owned()),
                            score: persisted
                                .as_ref()
                                .and_then(|candidate| candidate.score)
                                .or(Some(0.55)),
                            evidence: persisted
                                .as_ref()
                                .and_then(|candidate| {
                                    serde_json::from_str(&candidate.evidence_json).ok()
                                })
                                .unwrap_or_else(|| evidence.clone()),
                            target_person_id: persisted
                                .as_ref()
                                .and_then(|candidate| candidate.target_person_id.clone()),
                            previous_person_id: persisted
                                .as_ref()
                                .and_then(|candidate| candidate.previous_person_id.clone()),
                            created_at: current_people_unix_timestamp(),
                            updated_at: current_people_unix_timestamp(),
                            checksum: String::new(),
                        };
                        if let Err(error) =
                            self.persist_person_match_candidate_snapshot(snapshot).await
                        {
                            tracing::warn!(item_id, person_id = actor_id, %error, "could not persist ambiguous person match snapshot");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(item_id, person_id = actor_id, %error, "could not persist ambiguous person match");
                    }
                }
            }
            let bridge_person_key = (bridge_candidates.len() == 1)
                .then(|| bridge_candidates[0].person_key.as_deref())
                .flatten();
            let person_key = self
                .resolve_person_key(actor, &identities, bridge_person_key)
                .await?;
            let has_stable_identity = person_key.is_some();
            let assets = if has_stable_identity {
                self.persist_person_assets(
                    actor,
                    actor_provider,
                    actor_id,
                    person_key.as_deref(),
                    &identities,
                )
                .await
            } else {
                PersonAssetResult {
                    image_file: None,
                    pending_assets: Vec::new(),
                }
            };
            if has_stable_identity && !assets.pending_assets.is_empty() {
                pending_assets.push(actor_id.to_owned());
            }
            let lux_person_id = person_key
                .as_deref()
                .filter(|person_key| person_key.starts_with("lux-"))
                .map(str::to_owned);
            stored.push(StoredActor {
                id: has_stable_identity.then(|| actor_id.to_owned()),
                name: actor.name.trim().to_owned(),
                provider: if has_stable_identity {
                    actor_provider.to_owned()
                } else {
                    String::new()
                },
                person_key,
                lux_person_id,
                identities,
                character: actor
                    .character
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                order: actor.order,
                image_file: assets.image_file,
                pending_assets: assets.pending_assets,
                person: actor.person.clone(),
            });
        }

        stored.sort_by_key(|actor| actor.order.unwrap_or(i32::MAX));
        let relation = StoredPeopleRelation {
            schema_version: PEOPLE_RELATION_SCHEMA_VERSION,
            generation: previous_relation
                .as_ref()
                .map(|relation| relation.generation.saturating_add(1).max(1))
                .unwrap_or(1),
            source_fingerprint: source_fingerprint
                .filter(|fingerprint| !fingerprint.is_empty())
                .map(encode_fingerprint),
            item_id: Some(item_id.to_owned()),
            source_key: source_locator
                .as_ref()
                .map(|locator| stable_source_key(&locator.root_path, &locator.relative_path)),
            source_root: source_locator
                .as_ref()
                .map(|locator| locator.root_path.clone()),
            source_relative_path: source_locator
                .as_ref()
                .map(|locator| locator.relative_path.clone()),
            media_fingerprint: source_locator
                .as_ref()
                .and_then(|locator| locator.fingerprint.as_deref())
                .map(encode_fingerprint),
            media_size: source_locator.as_ref().map(|locator| locator.size),
            media_modified_at: source_locator.as_ref().map(|locator| locator.modified_at),
            media_title: source_locator.as_ref().map(|locator| locator.title.clone()),
            media_production_year: source_locator
                .as_ref()
                .and_then(|locator| locator.production_year),
            actors: stored,
        };
        let bytes = serde_json::to_vec_pretty(&relation)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        write_atomically(relation_path, &bytes).await?;
        if let Some(database) = &self.database {
            let credits = relation
                .actors
                .iter()
                .map(person_credit_from_stored_actor)
                .collect::<Vec<_>>();
            database
                .replace_person_credits_with_fingerprint(
                    item_id,
                    &credits,
                    relation.source_fingerprint.as_deref(),
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
        }
        Ok(ActorPersistReport {
            stored_count: relation.actors.len(),
            pending_assets,
        })
    }

    pub(super) async fn persist_person_assets(
        &self,
        actor: &ActorCredit,
        provider: &str,
        provider_id: &str,
        person_key: Option<&str>,
        identities: &[PersonIdentity],
    ) -> PersonAssetResult {
        let mut pending_assets = Vec::new();
        let person_dir = if let Some(person_key) = person_key.filter(|key| key.starts_with("lux-"))
        {
            match lux_person_directory(&self.config_dir, &actor.name, person_key) {
                Ok(person_dir) => person_dir,
                Err(error) => {
                    tracing::warn!(
                        person_id = %provider_id,
                        %error,
                        "actor person path was not prepared"
                    );
                    pending_assets.push(PENDING_PERSON_DIRECTORY.to_owned());
                    return PersonAssetResult {
                        image_file: None,
                        pending_assets,
                    };
                }
            }
        } else {
            match people_directory(&self.config_dir, &actor.name, provider, provider_id) {
                Ok(legacy_dir) => {
                    let path = if safe_metadata(&legacy_dir).await.ok().flatten().is_some() {
                        Ok(legacy_dir)
                    } else if let Some(person_key) = person_key {
                        canonical_person_directory(&self.config_dir, person_key)
                            .map_err(PeopleError::from)
                    } else {
                        Ok(legacy_dir)
                    };
                    match path {
                        Ok(person_dir) => person_dir,
                        Err(error) => {
                            tracing::warn!(
                                person_id = %provider_id,
                                %error,
                                "actor person path was not prepared"
                            );
                            pending_assets.push(PENDING_PERSON_DIRECTORY.to_owned());
                            return PersonAssetResult {
                                image_file: None,
                                pending_assets,
                            };
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        person_id = %provider_id,
                        %error,
                        "actor person path was not prepared"
                    );
                    pending_assets.push(PENDING_PERSON_DIRECTORY.to_owned());
                    return PersonAssetResult {
                        image_file: None,
                        pending_assets,
                    };
                }
            }
        };
        if let Err(error) = create_private_dir(&person_dir).await {
            tracing::warn!(
                person_id = %provider_id,
                %error,
                "actor person directory was not prepared"
            );
            pending_assets.push(PENDING_PERSON_DIRECTORY.to_owned());
            return PersonAssetResult {
                image_file: None,
                pending_assets,
            };
        }
        if person_key.is_some_and(|key| key.starts_with("lux-"))
            && let Err(error) = self
                .migrate_legacy_person_assets(&actor.name, provider, provider_id, &person_dir)
                .await
        {
            tracing::warn!(person_id = %provider_id, %error, "legacy person assets were not migrated");
            pending_assets.push(PENDING_PERSON_DIRECTORY.to_owned());
        }

        let nfo_path = person_dir.join(PERSON_NFO);
        let index_dir = people_index_directory(&self.config_dir);
        if let Err(error) = create_private_dir(&index_dir).await {
            tracing::warn!(
                person_id = %provider_id,
                %error,
                "actor people index directory was not prepared"
            );
        }
        let nfo_result = async {
            let bytes = self
                .person_nfo_bytes_with_existing(
                    &nfo_path,
                    &actor.name,
                    provider,
                    provider_id,
                    actor.person.as_ref(),
                    identities,
                )
                .await?;
            write_atomically(&nfo_path, &bytes).await
        }
        .await;
        if let Err(error) = nfo_result {
            tracing::warn!(person_id = %provider_id, %error, "actor person NFO was not cached");
            pending_assets.push(PENDING_PERSON_NFO.to_owned());
        }

        if let Some(person_key) = person_key.filter(|key| key.starts_with("lux-")) {
            if let Err(error) = self
                .persist_person_manifest(
                    &person_dir,
                    person_key,
                    &actor.name,
                    provider,
                    identities,
                    actor.person.as_ref(),
                )
                .await
            {
                tracing::warn!(person_id = %provider_id, %error, "actor person manifest was not cached");
                pending_assets.push(PENDING_PERSON_MANIFEST.to_owned());
            }
        }

        let image_file = match self
            .ensure_profile_image(
                provider_id,
                provider,
                actor.profile_url.as_deref(),
                &person_dir,
                person_key.is_some_and(|key| key.starts_with("lux-")),
            )
            .await
        {
            Ok(image_file) => image_file,
            Err(error) => {
                tracing::warn!(person_id = %provider_id, %error, "actor profile image was not cached");
                pending_assets.push(PENDING_PROFILE_IMAGE.to_owned());
                None
            }
        };
        let image_available = image_file.is_some()
            || matches!(
                self.profile_image_for_provider(Some(provider), provider_id)
                    .await,
                Ok(Some(_))
            );
        if !image_available {
            pending_assets.push(PENDING_PROFILE_IMAGE.to_owned());
        }

        if let Some(image_file) = image_file.as_deref()
            && !image_file.starts_with("legacy/")
        {
            let image_path = if image_file.starts_with("people/") {
                metadata_root(&self.config_dir).join(image_file)
            } else {
                person_dir.join(image_file)
            };
            let index_result = match image_path.strip_prefix(metadata_root(&self.config_dir)) {
                Ok(relative) => {
                    let index = StoredPersonIndex {
                        image_path: relative.to_string_lossy().into_owned(),
                        person_key: person_key.map(str::to_owned),
                    };
                    match serde_json::to_vec_pretty(&index) {
                        Ok(bytes) => {
                            let mut result = Ok(());
                            for identity in identities {
                                let index_path = match people_index_path_for_provider(
                                    &self.config_dir,
                                    &identity.provider,
                                    &identity.id,
                                ) {
                                    Ok(index_path) => index_path,
                                    Err(error) => {
                                        result = Err(PeopleError::from(error));
                                        break;
                                    }
                                };
                                if let Err(error) = write_atomically(&index_path, &bytes).await {
                                    result = Err(error);
                                    break;
                                }
                            }
                            result
                        }
                        Err(error) => Err(PeopleError::Serialization(error.to_string())),
                    }
                }
                Err(_) => Err(PeopleError::Serialization(
                    "person image path is outside metadata".to_owned(),
                )),
            };
            if let Err(error) = index_result {
                tracing::warn!(
                    person_id = %provider_id,
                    %error,
                    "actor person image index was not cached"
                );
                pending_assets.push(PENDING_PERSON_INDEX.to_owned());
            }
            if let Some(person_key) = person_key.filter(|key| key.starts_with("lux-"))
                && let Ok(index_path) = people_index_path(&self.config_dir, person_key)
            {
                let index = StoredPersonIndex {
                    image_path: image_path
                        .strip_prefix(metadata_root(&self.config_dir))
                        .ok()
                        .map(|relative| relative.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    person_key: Some(person_key.to_owned()),
                };
                if let Ok(bytes) = serde_json::to_vec_pretty(&index)
                    && write_atomically(&index_path, &bytes).await.is_err()
                {
                    pending_assets.push(PENDING_PERSON_INDEX.to_owned());
                }
            }
        }

        PersonAssetResult {
            image_file,
            pending_assets,
        }
    }

    pub(super) async fn migrate_legacy_person_assets(
        &self,
        display_name: &str,
        provider: &str,
        provider_id: &str,
        person_dir: &Path,
    ) -> Result<(), PeopleError> {
        let legacy_dir = people_directory(&self.config_dir, display_name, provider, provider_id)
            .map_err(PeopleError::from)?;
        self.migrate_person_assets_from_directory(&legacy_dir, person_dir)
            .await
    }

    pub(super) async fn migrate_person_assets_from_directory(
        &self,
        source_dir: &Path,
        person_dir: &Path,
    ) -> Result<(), PeopleError> {
        if source_dir == person_dir || safe_metadata(source_dir).await?.is_none() {
            return Ok(());
        }

        let legacy_nfo = source_dir.join(PERSON_NFO);
        let target_nfo = person_dir.join(PERSON_NFO);
        if safe_metadata(&target_nfo).await?.is_none()
            && let Some(bytes) = read_people_file(&legacy_nfo).await?
        {
            write_atomically(&target_nfo, &bytes).await?;
        }

        for extension in PROFILE_EXTENSIONS {
            let legacy_image = source_dir.join(format!("{PERSON_IMAGE}.{extension}"));
            let target_image = person_dir.join(format!("{PERSON_IMAGE}.{extension}"));
            if safe_metadata(&target_image).await?.is_none()
                && safe_metadata(&legacy_image)
                    .await?
                    .is_some_and(|metadata| metadata.is_file())
            {
                self.materialize_profile_asset(&legacy_image, person_dir)
                    .await?;
            }
        }
        Ok(())
    }

    pub(super) async fn persist_person_manifest(
        &self,
        person_dir: &Path,
        lux_person_id: &str,
        display_name: &str,
        source_provider: &str,
        identities: &[PersonIdentity],
        metadata: Option<&PersonMetadata>,
    ) -> Result<(), PeopleError> {
        let manifest_path = person_dir.join(PERSON_MANIFEST);
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
            let existing_checksum =
                (!manifest.checksum.is_empty()).then(|| manifest.checksum.clone());
            if !manifest.lux_person_id.is_empty() && manifest.lux_person_id != lux_person_id {
                return Err(PeopleError::Serialization(
                    "person manifest identity does not match directory".to_owned(),
                ));
            }
            manifest.schema_version = PERSON_MANIFEST_SCHEMA_VERSION;
            manifest.lux_person_id = lux_person_id.to_owned();
            if !display_name.trim().is_empty() {
                if !manifest.display_name.trim().is_empty()
                    && manifest.display_name != display_name.trim()
                {
                    manifest.aliases.insert(manifest.display_name.clone());
                }
                manifest.display_name = display_name.trim().to_owned();
            }
            for identity in identities {
                if !manifest
                    .identities
                    .iter()
                    .any(|existing| existing == identity)
                {
                    manifest.identities.push(identity.clone());
                    if !manifest.identity_events.iter().any(|event| {
                        event.event_type == "AUTO_PROVIDER_IDENTITY"
                            && event.provider == identity.provider
                            && event.provider_id == identity.id
                    }) {
                        manifest.identity_events.push(PersonManifestIdentityEvent {
                            event_id: Uuid::now_v7().to_string(),
                            event_type: "AUTO_PROVIDER_IDENTITY".to_owned(),
                            provider: identity.provider.clone(),
                            provider_id: identity.id.clone(),
                            from_person_id: None,
                            to_person_id: Some(lux_person_id.to_owned()),
                            evidence_json: serde_json::json!({
                                "method": "provider-identity",
                                "sourceProvider": source_provider,
                            })
                            .to_string(),
                            created_at: current_people_unix_timestamp(),
                        });
                    }
                }
            }
            manifest.identities.sort_by(|left, right| {
                left.provider
                    .cmp(&right.provider)
                    .then(left.id.cmp(&right.id))
            });
            if let Some(metadata) = metadata {
                for field in person_metadata_fields(metadata) {
                    manifest
                        .field_sources
                        .entry(field.to_owned())
                        .or_insert_with(|| source_provider.to_owned());
                }
                manifest.person = Some(
                    manifest
                        .person
                        .take()
                        .map(|existing| existing.supplement_missing_from(metadata.clone()))
                        .unwrap_or_else(|| metadata.clone()),
                );
            }
            manifest.checksum.clear();
            let checksum_source = serde_json::to_vec(&manifest)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            let digest = Sha256::digest(checksum_source);
            let candidate_checksum = digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            if existing_checksum.as_deref() == Some(candidate_checksum.as_str()) {
                return Ok(());
            }
            manifest.generation = manifest.generation.saturating_add(1).max(1);
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
