use super::*;

impl PeopleService {
    pub async fn list_libraries_actors(
        &self,
        library_ids: &[String],
        person_type: &str,
        options: PersonListOptions,
    ) -> Result<(Vec<ActorView>, i64), PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let (credits, total) = database
            .list_person_credits_for_libraries(library_ids, person_type, options)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        Ok((self.actor_views_from_credits(credits).await, total))
    }

    pub async fn search_actors(
        &self,
        library_ids: &[String],
        query: &str,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<ActorView>, i64), PeopleError> {
        let query = query.trim();
        if !is_valid_person_lookup(query) {
            return Err(PeopleError::InvalidComponent(query.to_owned()));
        }
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let (credits, total) = database
            .search_person_credits_for_libraries(library_ids, "Actor", query, offset, limit)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        Ok((self.actor_views_from_credits(credits).await, total))
    }

    pub async fn find_person(
        &self,
        library_ids: &[String],
        person_type: &str,
        person_id_or_name: &str,
    ) -> Result<Option<ActorView>, PeopleError> {
        if !is_valid_person_lookup(person_id_or_name) {
            return Err(PeopleError::InvalidComponent(person_id_or_name.to_owned()));
        }
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let credits = if is_valid_person_id(person_id_or_name) {
            database
                .find_person_credits_for_libraries(library_ids, person_type, person_id_or_name)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
        } else {
            Vec::new()
        };
        let credits = if credits.is_empty() {
            database
                .find_person_credits_for_libraries_by_name(
                    library_ids,
                    person_type,
                    person_id_or_name,
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
        } else {
            credits
        };
        Ok(self
            .actor_views_from_credits(credits)
            .await
            .into_iter()
            .next())
    }

    pub async fn update_person_metadata(
        &self,
        library_ids: &[String],
        person_id: &str,
        update: PersonMetadataUpdate,
    ) -> Result<bool, PeopleError> {
        self.update_person_metadata_with_nfo_mode(library_ids, person_id, update, false)
            .await
    }

    pub async fn replace_person_metadata(
        &self,
        library_ids: &[String],
        person_id: &str,
        update: PersonMetadataUpdate,
    ) -> Result<bool, PeopleError> {
        self.update_person_metadata_with_nfo_mode(library_ids, person_id, update, true)
            .await
    }

    pub(super) async fn update_person_metadata_with_nfo_mode(
        &self,
        library_ids: &[String],
        person_id: &str,
        update: PersonMetadataUpdate,
        replace_existing_nfo: bool,
    ) -> Result<bool, PeopleError> {
        if !is_valid_person_id(person_id) {
            return Err(PeopleError::InvalidComponent(person_id.to_owned()));
        }
        if !is_valid_person_lookup(&update.name) {
            return Err(PeopleError::InvalidComponent(update.name));
        }
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people index storage is unavailable".to_owned(),
            ));
        };
        let manifest_person_id = database
            .find_person_credits_for_libraries(library_ids, "Actor", person_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?
            .into_iter()
            .find_map(|credit| credit.lux_person_id)
            .unwrap_or_else(|| person_id.to_owned());
        let manifest_path = self
            .find_person_manifest_path(&manifest_person_id, &update.name)
            .await?;
        let (locked_fields, existing_metadata) = match read_people_file(&manifest_path).await? {
            Some(bytes) => {
                let manifest = serde_json::from_slice::<PersonManifest>(&bytes)
                    .map_err(|source| PeopleError::Serialization(source.to_string()))?;
                (manifest.locked_fields, manifest.person)
            }
            None => (BTreeSet::new(), None),
        };
        let item_ids = database
            .list_person_credit_item_ids(library_ids, "Actor", person_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let mut updated = false;
        for item_id in item_ids {
            let new_path = library_item_directory(&self.config_dir, &item_id)
                .map_err(PeopleError::from)?
                .join("people.json");
            let legacy_path = self
                .legacy_people_dir()
                .join(LEGACY_ITEMS_DIR)
                .join(format!("{item_id}.json"));
            let (relation_path, Some(mut relation)) = (match read_relation(&new_path).await? {
                Some(relation) => (new_path, Some(relation)),
                None => (legacy_path.clone(), read_relation(&legacy_path).await?),
            }) else {
                continue;
            };
            let mut item_updated = false;
            for actor in &mut relation.actors {
                if actor_id_from_stored_actor(actor) != person_id {
                    continue;
                }
                if !locked_fields.contains("name") || actor.name.trim().is_empty() {
                    actor.name = update.name.clone();
                }
                actor.person = Some(metadata_update_respecting_locks(
                    actor
                        .person
                        .as_ref()
                        .or(existing_metadata.as_ref())
                        .unwrap_or(&PersonMetadata::default()),
                    &update,
                    &locked_fields,
                ));
                if replace_existing_nfo {
                    self.write_person_nfo_for_actor_replacing_metadata(actor)
                        .await?;
                } else {
                    self.write_person_nfo_for_actor(actor).await?;
                }
                item_updated = true;
            }
            if !item_updated {
                continue;
            }
            let bytes = serde_json::to_vec_pretty(&relation)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            write_atomically(&relation_path, &bytes).await?;
            let credits = relation
                .actors
                .iter()
                .map(person_credit_from_stored_actor)
                .collect::<Vec<_>>();
            database
                .replace_person_credits_with_fingerprint(
                    &item_id,
                    &credits,
                    relation.source_fingerprint.as_deref(),
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
            updated = true;
        }
        Ok(updated)
    }

    pub(super) async fn write_person_nfo_for_actor(
        &self,
        actor: &StoredActor,
    ) -> Result<(), PeopleError> {
        self.write_person_nfo_for_actor_with_mode(actor, false)
            .await
    }

    pub(super) async fn write_person_nfo_for_actor_replacing_metadata(
        &self,
        actor: &StoredActor,
    ) -> Result<(), PeopleError> {
        self.write_person_nfo_for_actor_with_mode(actor, true).await
    }

    pub(super) async fn write_person_nfo_for_actor_with_mode(
        &self,
        actor: &StoredActor,
        replace_existing_metadata: bool,
    ) -> Result<(), PeopleError> {
        let person_id = actor_id_from_stored_actor(actor);
        let provider = if actor.provider.trim().is_empty() {
            actor
                .identities
                .iter()
                .find(|identity| identity.id == person_id)
                .map(|identity| identity.provider.as_str())
                .unwrap_or("local")
        } else {
            actor.provider.trim()
        };
        if !is_valid_person_id(&person_id) || !is_valid_person_id(provider) {
            return Err(PeopleError::InvalidComponent(person_id));
        }
        let person_dir = if let Some(person_key) = actor
            .person_key
            .as_deref()
            .filter(|person_key| person_key.starts_with("lux-"))
        {
            lux_person_directory(&self.config_dir, &actor.name, person_key)
                .map_err(PeopleError::from)?
        } else {
            let legacy_dir = people_directory(&self.config_dir, &actor.name, provider, &person_id)
                .map_err(PeopleError::from)?;
            if safe_metadata(&legacy_dir).await.ok().flatten().is_some() {
                legacy_dir
            } else if let Some(person_key) = actor.person_key.as_deref() {
                canonical_person_directory(&self.config_dir, person_key)
                    .map_err(PeopleError::from)?
            } else {
                legacy_dir
            }
        };
        create_private_dir(&person_dir).await?;
        let nfo_path = person_dir.join(PERSON_NFO);
        let bytes = if replace_existing_metadata {
            self.person_nfo_bytes_with_replaced_metadata(
                &nfo_path,
                &actor.name,
                provider,
                &person_id,
                actor.person.as_ref(),
            )
            .await?
        } else {
            self.person_nfo_bytes_with_existing(
                &nfo_path,
                &actor.name,
                provider,
                &person_id,
                actor.person.as_ref(),
                &[],
            )
            .await?
        };
        write_atomically(&nfo_path, &bytes).await
    }

    pub(super) async fn person_nfo_bytes_with_existing(
        &self,
        path: &Path,
        name: &str,
        provider: &str,
        provider_id: &str,
        metadata: Option<&PersonMetadata>,
        identities: &[PersonIdentity],
    ) -> Result<Vec<u8>, PeopleError> {
        let mut bytes = read_people_file(path)
            .await?
            .map(|existing| {
                merge_person_nfo_bytes(&existing, name, provider, provider_id, metadata)
                    .unwrap_or(existing)
            })
            .unwrap_or_else(|| person_nfo_bytes(name, provider, provider_id, metadata));
        for identity in identities {
            if identity.provider == provider && identity.id == provider_id {
                continue;
            }
            bytes = merge_person_nfo_bytes(&bytes, name, &identity.provider, &identity.id, None)
                .unwrap_or(bytes);
        }
        Ok(bytes)
    }

    pub(super) async fn person_nfo_bytes_with_replaced_metadata(
        &self,
        path: &Path,
        name: &str,
        provider: &str,
        provider_id: &str,
        metadata: Option<&PersonMetadata>,
    ) -> Result<Vec<u8>, PeopleError> {
        let Some(existing) = read_people_file(path).await? else {
            return Ok(person_nfo_bytes(name, provider, provider_id, metadata));
        };
        Ok(
            replace_person_nfo_bytes(&existing, name, provider, provider_id, metadata)
                .unwrap_or_else(|| person_nfo_bytes(name, provider, provider_id, metadata)),
        )
    }

    pub(super) async fn actor_views_from_credits(
        &self,
        credits: Vec<StoredPersonCredit>,
    ) -> Vec<ActorView> {
        let mut views = Vec::with_capacity(credits.len());
        for credit in credits {
            let lookup_id = credit
                .lux_person_id
                .clone()
                .unwrap_or_else(|| credit.person_id.clone());
            let provider = (!credit.provider.is_empty()).then(|| credit.provider.clone());
            let image_url = if let Some(lux_person_id) = credit.lux_person_id.as_deref()
                && matches!(self.profile_image(lux_person_id).await, Ok(Some(_)))
            {
                provider
                    .as_deref()
                    .and_then(|provider| actor_image_url(provider, &credit.person_id))
                    .or_else(|| Some(format!("/api/v1/people/{}/image", credit.person_id)))
            } else {
                self.person_image_url(provider.as_deref(), &credit.person_id)
                    .await
            };
            let stored_metadata = PersonMetadata {
                biography: credit.biography.clone(),
                birthday: credit.birthday.clone(),
                deathday: credit.deathday.clone(),
                known_for_department: credit.known_for_department.clone(),
                place_of_birth: credit.place_of_birth.clone(),
                provider_ids: credit.provider_ids.clone(),
                genres: credit.genres.clone(),
                tags: credit.tags.clone(),
                production_locations: credit.production_locations.clone(),
                premiere_date: credit.premiere_date.clone(),
                production_year: credit
                    .production_year
                    .and_then(|year| i32::try_from(year).ok()),
                taglines: credit.taglines.clone(),
            };
            let metadata = self
                .person_metadata_from_relation(&credit.item_id, &credit.person_id)
                .await
                .map(|relation_metadata| {
                    stored_metadata
                        .clone()
                        .supplement_missing_from(relation_metadata)
                })
                .unwrap_or(stored_metadata);
            views.push(ActorView {
                id: credit.person_id,
                lookup_id,
                provider,
                name: credit.person_name,
                character: (!credit.role.is_empty()).then_some(credit.role),
                is_favorite: false,
                date_created: Some(credit.date_created),
                image_url,
                biography: metadata.biography,
                birthday: metadata.birthday,
                deathday: metadata.deathday,
                known_for_department: metadata.known_for_department,
                place_of_birth: metadata.place_of_birth,
                provider_ids: metadata.provider_ids,
                genres: metadata.genres,
                tags: metadata.tags,
                production_locations: metadata.production_locations,
                premiere_date: metadata.premiere_date,
                production_year: metadata.production_year.map(i64::from),
                taglines: metadata.taglines,
            });
        }
        views
    }

    pub(super) async fn person_metadata_from_relation(
        &self,
        item_id: &str,
        person_id: &str,
    ) -> Option<PersonMetadata> {
        let path = library_item_directory(&self.config_dir, item_id)
            .ok()?
            .join("people.json");
        let legacy_path = self
            .legacy_people_dir()
            .join(LEGACY_ITEMS_DIR)
            .join(format!("{item_id}.json"));
        let relation = match read_relation(&path).await.ok()? {
            Some(relation) => relation,
            None => read_relation(&legacy_path).await.ok()??,
        };
        relation
            .actors
            .iter()
            .find(|actor| actor_id_from_stored_actor(actor) == person_id)
            .and_then(|actor| actor.person.clone())
    }

    pub(super) async fn person_image_url(
        &self,
        provider: Option<&str>,
        person_id: &str,
    ) -> Option<String> {
        let image = match provider {
            Some(provider) => {
                self.profile_image_for_provider(Some(provider), person_id)
                    .await
            }
            None => self.profile_image(person_id).await,
        };
        if !matches!(image, Ok(Some(_))) {
            return None;
        }
        provider
            .and_then(|provider| actor_image_url(provider, person_id))
            .or_else(|| Some(format!("/api/v1/people/{person_id}/image")))
    }
}
