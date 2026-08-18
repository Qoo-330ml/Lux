use std::{
    fmt,
    fmt::Write as _,
    path::{Component, Path, PathBuf},
};

use quick_xml::escape::escape;
use reqwest::{Client, Url, header::CONTENT_TYPE};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

use crate::application::metadata_paths::{
    MetadataPathError, canonical_person_directory, library_item_directory, metadata_root,
    people_directory, people_index_directory, people_index_path, people_index_path_for_provider,
    readable_component,
};
use crate::storage::{Database, NewPersonCredit, PersonListOptions, StoredPersonCredit};

const LEGACY_PEOPLE_DIR: &str = "people";
const LEGACY_ITEMS_DIR: &str = "items";
const LEGACY_PROFILES_DIR: &str = "profiles";
const PERSON_NFO: &str = "person.nfo";
const PERSON_IMAGE: &str = "folder";
const PEOPLE_RELATION_SCHEMA_VERSION: u32 = 2;
const PENDING_PERSON_DIRECTORY: &str = "personDirectory";
const PENDING_PERSON_NFO: &str = "personNfo";
const PENDING_PROFILE_IMAGE: &str = "profileImage";
const PENDING_PERSON_INDEX: &str = "personIndex";
const MAX_ACTORS: usize = 12;
const MAX_PEOPLE_FILE_BYTES: u64 = 256 * 1024;
const MAX_PROFILE_BYTES: usize = 10 * 1024 * 1024;
const PROFILE_EXTENSIONS: [&str; 3] = ["jpg", "png", "webp"];
const PERSON_INDEX_REBUILD_BATCH_SIZE: i64 = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorCredit {
    #[serde(default, deserialize_with = "deserialize_person_id")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<PersonIdentity>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<PersonMetadata>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biography: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birthday: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deathday: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_for_department: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_of_birth: Option<String>,
}

/// Metadata supplied by an Emby-compatible person update request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PersonMetadataUpdate {
    pub name: String,
    pub biography: Option<String>,
    pub birthday: Option<String>,
    pub deathday: Option<String>,
    pub known_for_department: Option<String>,
    pub place_of_birth: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonIdentity {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredActor {
    #[serde(default, deserialize_with = "deserialize_optional_person_id")]
    id: Option<String>,
    name: String,
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    person_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    identities: Vec<PersonIdentity>,
    #[serde(default)]
    character: Option<String>,
    #[serde(default)]
    order: Option<i32>,
    #[serde(default)]
    image_file: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_assets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    person: Option<PersonMetadata>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredPeopleRelation {
    #[serde(default = "default_relation_schema_version")]
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_fingerprint: Option<String>,
    #[serde(default)]
    actors: Vec<StoredActor>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActorPersistReport {
    pub stored_count: usize,
    pub pending_assets: Vec<String>,
}

struct PersonAssetResult {
    image_file: Option<String>,
    pending_assets: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredPersonIndex {
    image_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    person_key: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorView {
    pub id: String,
    pub provider: Option<String>,
    pub name: String,
    pub character: Option<String>,
    pub date_created: Option<i64>,
    pub image_url: Option<String>,
    pub biography: Option<String>,
    pub birthday: Option<String>,
    pub deathday: Option<String>,
    pub known_for_department: Option<String>,
    pub place_of_birth: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PersonImage {
    pub path: PathBuf,
    pub content_type: &'static str,
    pub content_length: u64,
}

#[derive(Clone)]
pub struct PeopleService {
    config_dir: PathBuf,
    client: Client,
    database: Option<Database>,
}

impl PeopleService {
    pub fn new(config_dir: PathBuf) -> Self {
        Self::with_proxy(config_dir, None)
    }

    pub fn new_with_proxy(config_dir: PathBuf, proxy_url: Option<String>) -> Self {
        Self::with_proxy(config_dir, proxy_url)
    }

    pub fn with_database(mut self, database: Database) -> Self {
        self.database = Some(database);
        self
    }

    fn with_proxy(config_dir: PathBuf, proxy_url: Option<String>) -> Self {
        let client = match crate::network::client_builder_from_env_or(proxy_url.as_deref()) {
            Ok(builder) => match builder.build() {
                Ok(client) => client,
                Err(_) => Client::new(),
            },
            Err(_) => Client::new(),
        };
        Self {
            config_dir,
            client,
            database: None,
        }
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

    async fn persist_item_actors_with_source(
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
            let person_key = person_key_for_identities(&identities);
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
            stored.push(StoredActor {
                id: has_stable_identity.then(|| actor_id.to_owned()),
                name: actor.name.trim().to_owned(),
                provider: if has_stable_identity {
                    actor_provider.to_owned()
                } else {
                    String::new()
                },
                person_key,
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
            source_fingerprint: source_fingerprint
                .filter(|fingerprint| !fingerprint.is_empty())
                .map(encode_fingerprint),
            actors: stored,
        };
        let bytes = serde_json::to_vec_pretty(&relation)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        write_atomically(&relation_path, &bytes).await?;
        if let Some(database) = &self.database {
            let credits = relation
                .actors
                .iter()
                .map(person_credit_from_stored_actor)
                .collect::<Vec<_>>();
            database
                .replace_person_credits(item_id, &credits)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
        }
        Ok(ActorPersistReport {
            stored_count: relation.actors.len(),
            pending_assets,
        })
    }

    async fn persist_person_assets(
        &self,
        actor: &ActorCredit,
        provider: &str,
        provider_id: &str,
        person_key: Option<&str>,
        identities: &[PersonIdentity],
    ) -> PersonAssetResult {
        let mut pending_assets = Vec::new();
        let person_dir =
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
                    let Ok(person_dir) = path else {
                        pending_assets.push(PENDING_PERSON_DIRECTORY.to_owned());
                        return PersonAssetResult {
                            image_file: None,
                            pending_assets,
                        };
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
                    person_dir
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
            };

        let nfo_path = person_dir.join(PERSON_NFO);
        let index_dir = people_index_directory(&self.config_dir);
        if let Err(error) = create_private_dir(&index_dir).await {
            tracing::warn!(
                person_id = %provider_id,
                %error,
                "actor people index directory was not prepared"
            );
        }
        if let Err(error) = write_atomically(
            &nfo_path,
            &person_nfo_bytes(&actor.name, provider, provider_id, actor.person.as_ref()),
        )
        .await
        {
            tracing::warn!(person_id = %provider_id, %error, "actor person NFO was not cached");
            pending_assets.push(PENDING_PERSON_NFO.to_owned());
        }

        let image_file = match self
            .ensure_profile_image(
                provider_id,
                provider,
                actor.profile_url.as_deref(),
                &person_dir,
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
        }

        PersonAssetResult {
            image_file,
            pending_assets,
        }
    }

    pub async fn item_actor_relation_exists(&self, item_id: &str) -> Result<bool, PeopleError> {
        let new_path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        if read_relation(&new_path).await?.is_some() {
            return Ok(true);
        }
        let legacy_path = self
            .legacy_people_dir()
            .join(LEGACY_ITEMS_DIR)
            .join(format!("{item_id}.json"));
        Ok(read_relation(&legacy_path).await?.is_some())
    }

    pub async fn item_actor_relation_is_current(
        &self,
        item_id: &str,
        source_fingerprint: &[u8],
    ) -> Result<bool, PeopleError> {
        let path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let Some(relation) = read_relation(&path).await? else {
            return Ok(false);
        };
        Ok(relation
            .source_fingerprint
            .as_deref()
            .and_then(decode_fingerprint)
            .is_some_and(|stored| stored == source_fingerprint))
    }

    pub async fn nfo_relation_snapshot_exists(&self, item_id: &str) -> Result<bool, PeopleError> {
        let path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let Some(relation) = read_relation(&path).await? else {
            return Ok(false);
        };
        Ok(relation
            .source_fingerprint
            .as_deref()
            .and_then(decode_fingerprint)
            .is_some_and(|fingerprint| fingerprint.len() == 32))
    }

    pub async fn list_item_actors(&self, item_id: &str) -> Result<Vec<ActorView>, PeopleError> {
        let new_path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let legacy_path = self
            .legacy_people_dir()
            .join(LEGACY_ITEMS_DIR)
            .join(format!("{item_id}.json"));
        let bytes = match read_people_file(&new_path).await? {
            Some(bytes) => bytes,
            None => match read_people_file(&legacy_path).await? {
                Some(bytes) => bytes,
                None => return Ok(Vec::new()),
            },
        };
        let relation = parse_relation(&bytes)?;
        let mut views = Vec::new();
        for actor in relation
            .actors
            .into_iter()
            .take(MAX_ACTORS)
            .filter(|actor| !actor.name.trim().is_empty())
        {
            let id = actor_id_from_stored_actor(&actor);
            let provider = actor_provider_from_stored_actor(&actor);
            let image_url = self.person_image_url(provider.as_deref(), &id).await;
            views.push(ActorView {
                id,
                provider,
                name: actor.name,
                character: actor.character,
                date_created: None,
                image_url,
                biography: actor
                    .person
                    .as_ref()
                    .and_then(|person| person.biography.clone()),
                birthday: actor
                    .person
                    .as_ref()
                    .and_then(|person| person.birthday.clone()),
                deathday: actor
                    .person
                    .as_ref()
                    .and_then(|person| person.deathday.clone()),
                known_for_department: actor
                    .person
                    .as_ref()
                    .and_then(|person| person.known_for_department.clone()),
                place_of_birth: actor
                    .person
                    .as_ref()
                    .and_then(|person| person.place_of_birth.clone()),
            });
        }
        Ok(views)
    }

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

    pub async fn rebuild_person_credit_index(&self) -> Result<usize, PeopleError> {
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        let library_ids = database
            .list_enabled_library_ids()
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let mut rebuilt_items = 0;
        for library_id in library_ids {
            let mut offset = 0;
            loop {
                let item_ids = database
                    .list_media_item_ids_for_library(
                        &library_id,
                        offset,
                        PERSON_INDEX_REBUILD_BATCH_SIZE,
                    )
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?;
                if item_ids.is_empty() {
                    break;
                }
                for item_id in &item_ids {
                    match self.rebuild_item_person_credit_index(item_id).await {
                        Ok(()) => rebuilt_items += 1,
                        Err(PeopleError::Serialization(message)) => {
                            tracing::warn!(item_id, %message, "skipping malformed people relation during index rebuild");
                        }
                        Err(error) => return Err(error),
                    }
                }
                offset += item_ids.len() as i64;
                if item_ids.len() < PERSON_INDEX_REBUILD_BATCH_SIZE as usize {
                    break;
                }
            }
        }
        Ok(rebuilt_items)
    }

    async fn rebuild_item_person_credit_index(&self, item_id: &str) -> Result<(), PeopleError> {
        let new_path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let legacy_path = self
            .legacy_people_dir()
            .join(LEGACY_ITEMS_DIR)
            .join(format!("{item_id}.json"));
        let relation = match read_relation(&new_path).await? {
            Some(relation) => Some(relation),
            None => read_relation(&legacy_path).await?,
        };
        let credits = relation
            .as_ref()
            .map(|relation| {
                relation
                    .actors
                    .iter()
                    .take(MAX_ACTORS)
                    .filter(|actor| !actor.name.trim().is_empty())
                    .map(person_credit_from_stored_actor)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let Some(database) = &self.database else {
            return Err(PeopleError::Storage(
                "people database index is unavailable".to_owned(),
            ));
        };
        database
            .replace_person_credits(item_id, &credits)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))
    }

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
                actor.name = update.name.clone();
                actor.person = Some(PersonMetadata {
                    biography: update.biography.clone(),
                    birthday: update.birthday.clone(),
                    deathday: update.deathday.clone(),
                    known_for_department: update.known_for_department.clone(),
                    place_of_birth: update.place_of_birth.clone(),
                });
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
                .replace_person_credits(&item_id, &credits)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
            updated = true;
        }
        Ok(updated)
    }

    async fn actor_views_from_credits(&self, credits: Vec<StoredPersonCredit>) -> Vec<ActorView> {
        let mut views = Vec::with_capacity(credits.len());
        for credit in credits {
            let provider = (!credit.provider.is_empty()).then(|| credit.provider.clone());
            let image_url = self
                .person_image_url(provider.as_deref(), &credit.person_id)
                .await;
            views.push(ActorView {
                id: credit.person_id,
                provider,
                name: credit.person_name,
                character: (!credit.role.is_empty()).then_some(credit.role),
                date_created: Some(credit.date_created),
                image_url,
                biography: credit.biography,
                birthday: credit.birthday,
                deathday: credit.deathday,
                known_for_department: credit.known_for_department,
                place_of_birth: credit.place_of_birth,
            });
        }
        views
    }

    async fn person_image_url(&self, provider: Option<&str>, person_id: &str) -> Option<String> {
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

    pub async fn profile_image(&self, person_id: &str) -> Result<Option<PersonImage>, PeopleError> {
        self.profile_image_for_provider(None, person_id).await
    }

    pub async fn update_person_image(
        &self,
        person_id: &str,
        content_type: Option<&str>,
        bytes: &[u8],
    ) -> Result<(), PeopleError> {
        validate_component(person_id)?;
        if bytes.is_empty() || bytes.len() > MAX_PROFILE_BYTES {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }
        let (extension, expected_type) =
            profile_image_format(content_type, bytes).ok_or_else(|| {
                PeopleError::InvalidImage("unsupported profile image type".to_owned())
            })?;
        if !valid_image(expected_type, bytes) {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }
        let image_path = self.store_shared_profile_asset(bytes, extension).await?;
        let index_path =
            people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?;
        create_private_dir(&people_index_directory(&self.config_dir)).await?;
        let index = StoredPersonIndex {
            image_path,
            person_key: None,
        };
        let index_bytes = serde_json::to_vec_pretty(&index)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        write_atomically(&index_path, &index_bytes).await
    }

    pub async fn profile_image_for_emby_name_or_id(
        &self,
        name_or_id: &str,
    ) -> Result<Option<PersonImage>, PeopleError> {
        if validate_component(name_or_id).is_ok()
            && let Some(image) = self.profile_image(name_or_id).await?
        {
            return Ok(Some(image));
        }
        self.profile_image_for_name(name_or_id).await
    }

    async fn profile_image_for_name(&self, name: &str) -> Result<Option<PersonImage>, PeopleError> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 128
            || name.contains('/')
            || name.contains('\\')
            || matches!(name, "." | "..")
        {
            return Err(PeopleError::InvalidComponent(name.to_owned()));
        }
        let people_root = metadata_root(&self.config_dir).join(LEGACY_PEOPLE_DIR);
        let mut buckets = match fs::read_dir(&people_root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(PeopleError::Io {
                    path: people_root,
                    source,
                });
            }
        };
        let prefix = format!("{}-", readable_component(name));
        while let Some(bucket) = buckets
            .next_entry()
            .await
            .map_err(|source| PeopleError::Io {
                path: people_root.clone(),
                source,
            })?
        {
            let bucket_path = bucket.path();
            if safe_metadata(&bucket_path)
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
            while let Some(person) =
                persons
                    .next_entry()
                    .await
                    .map_err(|source| PeopleError::Io {
                        path: bucket_path.clone(),
                        source,
                    })?
            {
                let person_path = person.path();
                if safe_metadata(&person_path)
                    .await?
                    .is_none_or(|metadata| !metadata.is_dir())
                {
                    continue;
                }
                let Some(person_name) = person.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if !person_name.starts_with(&prefix) {
                    continue;
                }
                for extension in PROFILE_EXTENSIONS {
                    if let Some(image) =
                        image_from_path(&person_path.join(format!("{PERSON_IMAGE}.{extension}")))
                            .await?
                    {
                        return Ok(Some(image));
                    }
                }
            }
        }
        Ok(None)
    }

    pub async fn profile_image_for_provider(
        &self,
        provider: Option<&str>,
        person_id: &str,
    ) -> Result<Option<PersonImage>, PeopleError> {
        validate_component(person_id)?;
        if let Some(provider) = provider {
            validate_component(provider)?;
            let index_path = people_index_path_for_provider(&self.config_dir, provider, person_id)
                .map_err(PeopleError::from)?;
            if let Some(image) = self.read_indexed_person_image(&index_path).await? {
                return Ok(Some(image));
            }
            if provider.eq_ignore_ascii_case("tmdb") {
                let legacy_index_path =
                    people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?;
                if let Some(image) = self.read_indexed_person_image(&legacy_index_path).await? {
                    return Ok(Some(image));
                }
            }
            if !provider.eq_ignore_ascii_case("tmdb") {
                return Ok(None);
            }
        } else {
            let legacy_index_path =
                people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?;
            if let Some(image) = self.read_indexed_person_image(&legacy_index_path).await? {
                return Ok(Some(image));
            }
            let tmdb_index_path =
                people_index_path_for_provider(&self.config_dir, "tmdb", person_id)
                    .map_err(PeopleError::from)?;
            if let Some(image) = self.read_indexed_person_image(&tmdb_index_path).await? {
                return Ok(Some(image));
            }
        }

        let profiles_dir = self.legacy_people_dir().join(LEGACY_PROFILES_DIR);
        for extension in PROFILE_EXTENSIONS {
            let path = profiles_dir.join(format!("{person_id}.{extension}"));
            if let Some(image) = image_from_path(&path).await? {
                return Ok(Some(image));
            }
        }
        Ok(None)
    }

    async fn read_indexed_person_image(
        &self,
        index_path: &Path,
    ) -> Result<Option<PersonImage>, PeopleError> {
        let Some(index_bytes) = read_people_file(index_path).await? else {
            return Ok(None);
        };
        let index = serde_json::from_slice::<StoredPersonIndex>(&index_bytes)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        let relative = Path::new(&index.image_path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(PeopleError::Serialization(
                "person image index contains an unsafe path".to_owned(),
            ));
        }
        let metadata_dir = metadata_root(&self.config_dir);
        let path = metadata_dir.join(relative);
        if !path.starts_with(&metadata_dir) {
            return Err(PeopleError::Serialization(
                "person image path is outside metadata".to_owned(),
            ));
        }
        image_from_path(&path).await
    }

    fn legacy_people_dir(&self) -> PathBuf {
        self.config_dir.join(LEGACY_PEOPLE_DIR)
    }

    async fn ensure_profile_image(
        &self,
        person_id: &str,
        provider: &str,
        image_url: Option<&str>,
        person_dir: &Path,
    ) -> Result<Option<String>, PeopleError> {
        for extension in PROFILE_EXTENSIONS {
            let path = person_dir.join(format!("{PERSON_IMAGE}.{extension}"));
            if safe_metadata(&path)
                .await?
                .is_some_and(|metadata| metadata.is_file())
            {
                return Ok(Some(format!("{PERSON_IMAGE}.{extension}")));
            }
        }

        let legacy_profiles_dir = self.legacy_people_dir().join(LEGACY_PROFILES_DIR);
        if provider.eq_ignore_ascii_case("tmdb") {
            for extension in PROFILE_EXTENSIONS {
                let path = legacy_profiles_dir.join(format!("{person_id}.{extension}"));
                if safe_metadata(&path)
                    .await?
                    .is_some_and(|metadata| metadata.is_file())
                {
                    return Ok(Some(format!("legacy/{person_id}.{extension}")));
                }
            }
        }

        // The provider ID is the identity key. If another title already
        // indexed a local image for this person, reuse it instead of
        // downloading the same profile again. The relation keeps
        // `imageFile` empty in this case; `profile_image` resolves the
        // canonical index independently of the title-specific directory.
        if let Ok(Some(image)) = self
            .profile_image_for_provider(Some(provider), person_id)
            .await
        {
            if let Ok(relative) = image.path.strip_prefix(metadata_root(&self.config_dir)) {
                return Ok(Some(relative.to_string_lossy().into_owned()));
            }
            return Ok(None);
        }

        let Some(image_url) = image_url else {
            return Ok(None);
        };

        let url =
            Url::parse(image_url).map_err(|source| PeopleError::InvalidUrl(source.to_string()))?;
        if url.scheme() != "https"
            || url.host_str().is_none_or(str::is_empty)
            || url.path().is_empty()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(PeopleError::InvalidUrl(
                "actor profile URL must be a valid HTTPS scraper image URL".to_owned(),
            ));
        }
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|source| PeopleError::Download(source.to_string()))?;
        if !response.status().is_success() {
            return Err(PeopleError::UpstreamStatus(response.status().as_u16()));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .ok_or_else(|| {
                PeopleError::InvalidImage("profile image content type is missing".to_owned())
            })?;
        let (extension, expected_type) = match content_type {
            "image/jpeg" => ("jpg", "image/jpeg"),
            "image/png" => ("png", "image/png"),
            "image/webp" => ("webp", "image/webp"),
            other => {
                return Err(PeopleError::InvalidImage(format!(
                    "unsupported profile image type: {other}"
                )));
            }
        };
        let bytes = response
            .bytes()
            .await
            .map_err(|source| PeopleError::Download(source.to_string()))?;
        if bytes.is_empty()
            || bytes.len() > MAX_PROFILE_BYTES
            || !valid_image(expected_type, &bytes)
        {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }
        Ok(Some(
            self.store_shared_profile_asset(&bytes, extension).await?,
        ))
    }

    async fn store_shared_profile_asset(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<String, PeopleError> {
        if !PROFILE_EXTENSIONS.contains(&extension) {
            return Err(PeopleError::InvalidImage(
                "unsupported profile image extension".to_owned(),
            ));
        }
        let digest = Sha256::digest(bytes);
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            let _ = write!(encoded, "{byte:02x}");
        }
        let relative = format!("people/assets/{encoded}.{extension}");
        let path = metadata_root(&self.config_dir).join(&relative);
        let parent = path.parent().ok_or_else(|| {
            PeopleError::Serialization("profile asset path has no parent".to_owned())
        })?;
        create_private_dir(parent).await?;
        if safe_metadata(&path).await?.is_none() {
            write_atomically(&path, bytes).await?;
        }
        Ok(relative)
    }
}

async fn read_relation(path: &Path) -> Result<Option<StoredPeopleRelation>, PeopleError> {
    let Some(bytes) = read_people_file(path).await? else {
        return Ok(None);
    };
    parse_relation(&bytes).map(Some)
}

fn parse_relation(bytes: &[u8]) -> Result<StoredPeopleRelation, PeopleError> {
    if bytes.len() as u64 > MAX_PEOPLE_FILE_BYTES {
        return Err(PeopleError::Serialization(
            "people data is too large".to_owned(),
        ));
    }
    let value = serde_json::from_slice::<Value>(bytes)
        .map_err(|source| PeopleError::Serialization(source.to_string()))?;
    if value.is_array() {
        let actors = serde_json::from_value::<Vec<StoredActor>>(value)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        return Ok(StoredPeopleRelation {
            schema_version: 0,
            source_fingerprint: None,
            actors,
        });
    }
    let relation = serde_json::from_value::<StoredPeopleRelation>(value)
        .map_err(|source| PeopleError::Serialization(source.to_string()))?;
    if relation.schema_version > PEOPLE_RELATION_SCHEMA_VERSION {
        return Err(PeopleError::Serialization(
            "people data schema is newer than supported".to_owned(),
        ));
    }
    Ok(relation)
}

fn default_relation_schema_version() -> u32 {
    PEOPLE_RELATION_SCHEMA_VERSION
}

fn encode_fingerprint(fingerprint: &[u8]) -> String {
    let mut encoded = String::with_capacity(fingerprint.len().saturating_mul(2));
    for byte in fingerprint {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_fingerprint(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() % 2 != 0 {
        return None;
    }
    let mut decoded = Vec::with_capacity(value.len() / 2);
    let bytes = value.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        decoded.push(((high << 4) | low) as u8);
    }
    Some(decoded)
}

async fn read_people_file(path: &Path) -> Result<Option<Vec<u8>>, PeopleError> {
    let Some(metadata) = safe_metadata(path).await? else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Err(PeopleError::Serialization(
            "people data path is not a file".to_owned(),
        ));
    }
    if metadata.len() > MAX_PEOPLE_FILE_BYTES {
        return Err(PeopleError::Serialization(
            "people data is too large".to_owned(),
        ));
    }
    fs::read(path)
        .await
        .map(Some)
        .map_err(|source| PeopleError::Io {
            path: path.to_owned(),
            source,
        })
}

async fn image_from_path(path: &Path) -> Result<Option<PersonImage>, PeopleError> {
    let Some(metadata) = safe_metadata(path).await? else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    let content_type = match path.extension().and_then(|value| value.to_str()) {
        Some("jpg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => return Ok(None),
    };
    Ok(Some(PersonImage {
        path: path.to_owned(),
        content_type,
        content_length: metadata.len(),
    }))
}

fn person_nfo_bytes(
    name: &str,
    provider: &str,
    provider_id: &str,
    metadata: Option<&PersonMetadata>,
) -> Vec<u8> {
    let metadata = metadata.map(person_metadata_xml).unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <person><name>{}</name>{}<uniqueid type=\"{}\">{}</uniqueid></person>\n",
        escape(name),
        metadata,
        escape(provider),
        escape(provider_id),
    )
    .into_bytes()
}

fn person_metadata_xml(metadata: &PersonMetadata) -> String {
    let mut xml = String::new();
    for (tag, value) in [
        ("biography", metadata.biography.as_deref()),
        ("birthday", metadata.birthday.as_deref()),
        ("deathday", metadata.deathday.as_deref()),
        ("knownfor", metadata.known_for_department.as_deref()),
        ("placeofbirth", metadata.place_of_birth.as_deref()),
    ] {
        if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
            xml.push_str(&format!("<{tag}>{}</{tag}>", escape(value)));
        }
    }
    xml
}

fn default_provider() -> String {
    "tmdb".to_owned()
}

fn actor_id_from_stored_actor(actor: &StoredActor) -> String {
    actor
        .id
        .as_deref()
        .filter(|id| is_valid_person_id(id))
        .map(str::to_owned)
        .or_else(|| {
            actor
                .identities
                .iter()
                .find(|identity| {
                    is_valid_person_id(&identity.provider) && is_valid_person_id(&identity.id)
                })
                .map(|identity| identity.id.clone())
        })
        .unwrap_or_else(|| local_actor_id(&actor.name, actor.character.as_deref()))
}

fn actor_provider_from_stored_actor(actor: &StoredActor) -> Option<String> {
    if actor.id.as_deref().is_some_and(is_valid_person_id)
        && !actor.provider.is_empty()
        && validate_component(&actor.provider).is_ok()
    {
        return Some(actor.provider.clone());
    }
    actor
        .identities
        .iter()
        .find(|identity| is_valid_person_id(&identity.provider) && is_valid_person_id(&identity.id))
        .map(|identity| identity.provider.clone())
}

fn person_credit_from_stored_actor(actor: &StoredActor) -> NewPersonCredit {
    NewPersonCredit {
        person_id: actor_id_from_stored_actor(actor),
        person_type: "Actor".to_owned(),
        person_name: actor.name.clone(),
        provider: actor_provider_from_stored_actor(actor).unwrap_or_default(),
        role: actor.character.clone().unwrap_or_default(),
        sort_order: i64::from(actor.order.unwrap_or(i32::MAX)),
        biography: actor
            .person
            .as_ref()
            .and_then(|person| person.biography.clone()),
        birthday: actor
            .person
            .as_ref()
            .and_then(|person| person.birthday.clone()),
        deathday: actor
            .person
            .as_ref()
            .and_then(|person| person.deathday.clone()),
        known_for_department: actor
            .person
            .as_ref()
            .and_then(|person| person.known_for_department.clone()),
        place_of_birth: actor
            .person
            .as_ref()
            .and_then(|person| person.place_of_birth.clone()),
    }
}

fn actor_image_url(provider: &str, person_id: &str) -> Option<String> {
    if provider.eq_ignore_ascii_case("tmdb") {
        Some(format!("/api/v1/people/{person_id}/image"))
    } else if validate_component(provider).is_ok() {
        Some(format!("/api/v1/people/{provider}/{person_id}/image"))
    } else {
        None
    }
}

fn validate_component(value: &str) -> Result<(), PeopleError> {
    if value.is_empty()
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(PeopleError::InvalidComponent(value.to_owned()));
    }
    Ok(())
}

fn is_valid_person_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn is_valid_person_lookup(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.contains('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && !matches!(value, "." | "..")
}

fn actor_identities(actor: &ActorCredit, fallback_provider: &str) -> Vec<PersonIdentity> {
    let mut identities = actor
        .identities
        .iter()
        .filter_map(|identity| {
            let provider = identity.provider.trim().to_ascii_lowercase();
            let id = identity.id.trim().to_owned();
            (is_valid_person_id(&provider) && is_valid_person_id(&id))
                .then_some(PersonIdentity { provider, id })
        })
        .collect::<Vec<_>>();
    let provider = actor
        .provider
        .as_deref()
        .unwrap_or(fallback_provider)
        .trim()
        .to_ascii_lowercase();
    let id = actor.id.trim();
    if is_valid_person_id(&provider)
        && is_valid_person_id(id)
        && !identities
            .iter()
            .any(|identity| identity.provider == provider && identity.id == id)
    {
        identities.push(PersonIdentity {
            provider,
            id: id.to_owned(),
        });
    }
    identities.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then(left.id.cmp(&right.id))
    });
    identities.dedup_by(|left, right| left.provider == right.provider && left.id == right.id);
    identities
}

fn person_key_for_identities(identities: &[PersonIdentity]) -> Option<String> {
    if identities.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    for identity in identities {
        hasher.update(identity.provider.as_bytes());
        hasher.update(*b":");
        hasher.update(identity.id.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    Some(format!("person-{encoded}"))
}

fn local_actor_id(name: &str, character: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.trim().as_bytes());
    hasher.update([0]);
    hasher.update(character.unwrap_or_default().trim().as_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    format!("local-{encoded}")
}

fn deserialize_person_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(value) => Ok(value),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(serde::de::Error::custom(
            "person ID must be a string or number",
        )),
    }
}

fn deserialize_optional_person_id<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value)),
        Value::Number(value) => Ok(Some(value.to_string())),
        _ => Err(serde::de::Error::custom(
            "person ID must be null, a string, or a number",
        )),
    }
}

async fn safe_metadata(path: &Path) -> Result<Option<std::fs::Metadata>, PeopleError> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(PeopleError::Symlink(path.to_owned()))
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PeopleError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

async fn create_private_dir(path: &Path) -> Result<(), PeopleError> {
    let mut current = Some(path.to_owned());
    while let Some(candidate) = current {
        if let Some(metadata) = safe_metadata(&candidate).await? {
            if !metadata.is_dir() {
                return Err(PeopleError::Io {
                    path: candidate,
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "metadata path component is not a directory",
                    ),
                });
            }
            break;
        }
        current = candidate.parent().map(Path::to_owned);
    }
    fs::create_dir_all(path)
        .await
        .map_err(|source| PeopleError::Io {
            path: path.to_owned(),
            source,
        })?;
    restrict_permissions(path, true).await
}

async fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), PeopleError> {
    let parent = path.parent().ok_or_else(|| PeopleError::Io {
        path: path.to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing parent"),
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| PeopleError::Io {
            path: path.to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid file name"),
        })?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = fs::File::create(&temporary)
            .await
            .map_err(|source| PeopleError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(bytes)
            .await
            .map_err(|source| PeopleError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.sync_all().await.map_err(|source| PeopleError::Io {
            path: temporary.clone(),
            source,
        })?;
        drop(file);
        fs::rename(&temporary, path)
            .await
            .map_err(|source| PeopleError::Io {
                path: path.to_owned(),
                source,
            })?;
        restrict_permissions(path, false).await
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

async fn restrict_permissions(path: &Path, directory: bool) -> Result<(), PeopleError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if directory { 0o700 } else { 0o600 };
        fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .await
            .map_err(|source| PeopleError::Io {
                path: path.to_owned(),
                source,
            })?;
    }
    #[cfg(not(unix))]
    let _ = (path, directory);
    Ok(())
}

fn valid_image(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

#[derive(Debug)]
pub enum PeopleError {
    InvalidComponent(String),
    MetadataPath(String),
    InvalidUrl(String),
    InvalidImage(String),
    UpstreamStatus(u16),
    Download(String),
    Serialization(String),
    Storage(String),
    Symlink(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for PeopleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidComponent(_) => formatter.write_str("invalid people path component"),
            Self::MetadataPath(message) => formatter.write_str(message),
            Self::InvalidUrl(message) | Self::InvalidImage(message) | Self::Download(message) => {
                formatter.write_str(message)
            }
            Self::UpstreamStatus(status) => {
                write!(formatter, "people image upstream returned {status}")
            }
            Self::Serialization(message) => write!(formatter, "people data is invalid: {message}"),
            Self::Storage(message) => write!(formatter, "people index storage failed: {message}"),
            Self::Symlink(path) => {
                write!(formatter, "people path is a symlink: {}", path.display())
            }
            Self::Io { path, source } => {
                write!(formatter, "people file {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for PeopleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidComponent(_)
            | Self::MetadataPath(_)
            | Self::InvalidUrl(_)
            | Self::InvalidImage(_)
            | Self::UpstreamStatus(_)
            | Self::Download(_)
            | Self::Serialization(_)
            | Self::Storage(_)
            | Self::Symlink(_) => None,
        }
    }
}

impl From<MetadataPathError> for PeopleError {
    fn from(error: MetadataPathError) -> Self {
        Self::MetadataPath(error.to_string())
    }
}

fn profile_image_format(
    content_type: Option<&str>,
    bytes: &[u8],
) -> Option<(&'static str, &'static str)> {
    let content_type = content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    match content_type.as_deref() {
        Some("image/jpeg") | Some("image/jpg") if valid_image("image/jpeg", bytes) => {
            Some(("jpg", "image/jpeg"))
        }
        Some("image/png") if valid_image("image/png", bytes) => Some(("png", "image/png")),
        Some("image/webp") if valid_image("image/webp", bytes) => Some(("webp", "image/webp")),
        Some(_) => None,
        None if valid_image("image/jpeg", bytes) => Some(("jpg", "image/jpeg")),
        None if valid_image("image/png", bytes) => Some(("png", "image/png")),
        None if valid_image("image/webp", bytes) => Some(("webp", "image/webp")),
        None => None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{ActorCredit, PeopleError, PeopleService, PersonIdentity};
    use crate::application::metadata_paths::{
        canonical_person_directory, library_item_directory, people_directory,
        people_index_path_for_provider,
    };

    #[tokio::test]
    async fn profile_image_rejects_symlinked_files() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let config = tempfile::tempdir()?;
        let profiles = config.path().join("people/profiles");
        tokio::fs::create_dir_all(&profiles).await?;
        let outside = config.path().join("outside.png");
        tokio::fs::write(&outside, b"not an image").await?;
        symlink(&outside, profiles.join("9.png"))?;

        let error = PeopleService::new(config.path().to_owned())
            .profile_image("9")
            .await
            .expect_err("symlinked profile must be rejected");
        assert!(matches!(error, PeopleError::Symlink(_)));
        Ok(())
    }

    #[tokio::test]
    async fn persist_writes_the_unified_person_layout() -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let person_dir = people_directory(config.path(), "演员甲", "TMDb", "9")?;
        tokio::fs::create_dir_all(&person_dir).await?;
        tokio::fs::write(person_dir.join("folder.png"), b"image").await?;

        let service = PeopleService::new(config.path().to_owned());
        let count = service
            .persist_item_actors(
                "item-1",
                "TMDb",
                &[ActorCredit {
                    id: "9".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色甲".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                }],
            )
            .await?;
        assert_eq!(count, 1);
        assert!(person_dir.join("person.nfo").exists());

        let relation = library_item_directory(config.path(), "item-1")?.join("people.json");
        let relation: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(relation).await?)?;
        assert_eq!(relation["schemaVersion"], 2);
        assert_eq!(relation["actors"][0]["provider"], "tmdb");
        assert_eq!(relation["actors"][0]["imageFile"], "folder.png");

        let index: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(people_index_path_for_provider(config.path(), "tmdb", "9")?).await?,
        )?;
        assert_eq!(index["imagePath"], "people/演/演员甲-tmdb-9/folder.png");
        assert_eq!(
            service.profile_image("9").await?.map(|image| image.path),
            Some(person_dir.join("folder.png"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn persist_writes_available_person_biography_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let person_dir = people_directory(config.path(), "演员甲", "tmdb", "9")?;
        tokio::fs::create_dir_all(&person_dir).await?;

        PeopleService::new(config.path().to_owned())
            .persist_item_actors(
                "item-biography",
                "tmdb",
                &[ActorCredit {
                    id: "9".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色甲".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: Some(super::PersonMetadata {
                        biography: Some("演员甲的生平介绍".to_owned()),
                        birthday: Some("1970-01-01".to_owned()),
                        deathday: None,
                        known_for_department: Some("Acting".to_owned()),
                        place_of_birth: Some("测试城市".to_owned()),
                    }),
                }],
            )
            .await?;

        let nfo = tokio::fs::read_to_string(person_dir.join("person.nfo")).await?;
        assert!(nfo.contains("<biography>演员甲的生平介绍</biography>"));
        assert!(nfo.contains("<birthday>1970-01-01</birthday>"));
        assert!(nfo.contains("<knownfor>Acting</knownfor>"));
        assert!(nfo.contains("<placeofbirth>测试城市</placeofbirth>"));
        Ok(())
    }

    #[tokio::test]
    async fn provider_scoped_people_images_do_not_collide_on_numeric_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let tmdb_dir = people_directory(config.path(), "甲演员", "tmdb", "9")?;
        let imdb_dir = people_directory(config.path(), "乙演员", "imdb", "9")?;
        tokio::fs::create_dir_all(&tmdb_dir).await?;
        tokio::fs::create_dir_all(&imdb_dir).await?;
        tokio::fs::write(tmdb_dir.join("folder.png"), b"tmdb-image").await?;
        tokio::fs::write(imdb_dir.join("folder.png"), b"imdb-image").await?;

        let service = PeopleService::new(config.path().to_owned());
        for (item_id, provider, name) in [
            ("item-tmdb", "tmdb", "甲演员"),
            ("item-imdb", "imdb", "乙演员"),
        ] {
            service
                .persist_item_actors(
                    item_id,
                    provider,
                    &[ActorCredit {
                        id: "9".to_owned(),
                        provider: None,
                        identities: Vec::new(),
                        name: name.to_owned(),
                        character: None,
                        order: Some(0),
                        profile_url: None,
                        person: None,
                    }],
                )
                .await?;
        }

        let tmdb = service
            .profile_image_for_provider(Some("tmdb"), "9")
            .await?
            .ok_or("missing tmdb image")?;
        let imdb = service
            .profile_image_for_provider(Some("imdb"), "9")
            .await?
            .ok_or("missing imdb image")?;
        assert_eq!(tmdb.path, tmdb_dir.join("folder.png"));
        assert_eq!(imdb.path, imdb_dir.join("folder.png"));
        assert_ne!(tmdb.path, imdb.path);
        Ok(())
    }

    #[tokio::test]
    async fn list_reads_the_legacy_relationship_layout() -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let legacy_items = config.path().join("people/items");
        tokio::fs::create_dir_all(&legacy_items).await?;
        tokio::fs::write(
            legacy_items.join("item-1.json"),
            r#"[{"id":"9","name":"旧演员","character":"旧角色","order":0,"imageFile":null}]"#
                .as_bytes(),
        )
        .await?;

        let actors = PeopleService::new(config.path().to_owned())
            .list_item_actors("item-1")
            .await?;
        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0].name, "旧演员");
        assert_eq!(actors[0].character.as_deref(), Some("旧角色"));
        Ok(())
    }

    #[tokio::test]
    async fn persist_rejects_a_symlinked_metadata_parent() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::symlink;

        let config = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        symlink(outside.path(), config.path().join("metadata"))?;
        let error = PeopleService::new(config.path().to_owned())
            .persist_item_actors(
                "item-1",
                "TMDb",
                &[ActorCredit {
                    id: "9".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: None,
                    order: None,
                    profile_url: None,
                    person: None,
                }],
            )
            .await
            .expect_err("symlinked metadata parent must be rejected");
        assert!(matches!(error, PeopleError::Symlink(_)));
        Ok(())
    }

    #[tokio::test]
    async fn local_actor_without_provider_id_is_kept_without_person_assets()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let count = service
            .persist_item_actors(
                "item-local",
                "local",
                &[ActorCredit {
                    id: String::new(),
                    provider: None,
                    identities: Vec::new(),
                    name: "本地演员".to_owned(),
                    character: Some("本地角色".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                }],
            )
            .await?;

        assert_eq!(count, 1);
        let relation = library_item_directory(config.path(), "item-local")?.join("people.json");
        let relation: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(relation).await?)?;
        assert_eq!(relation["actors"][0]["name"], "本地演员");
        assert!(relation["actors"][0]["id"].is_null());
        assert!(!config.path().join("metadata/people").exists());

        let actors = service.list_item_actors("item-local").await?;
        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0].name, "本地演员");
        assert!(actors[0].image_url.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn profile_assets_are_content_addressed_and_reused()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let first = service
            .store_shared_profile_asset(b"same-image", "png")
            .await?;
        let second = service
            .store_shared_profile_asset(b"same-image", "png")
            .await?;

        assert_eq!(first, second);
        let shared_path = config.path().join("metadata").join(&first);
        assert_eq!(tokio::fs::read(shared_path).await?, b"same-image");
        Ok(())
    }

    #[tokio::test]
    async fn multiple_provider_identities_share_one_person_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let identities = vec![
            PersonIdentity {
                provider: "tmdb".to_owned(),
                id: "124".to_owned(),
            },
            PersonIdentity {
                provider: "imdb".to_owned(),
                id: "nm123".to_owned(),
            },
        ];
        service
            .persist_item_actors(
                "item-multi-provider",
                "tmdb",
                &[ActorCredit {
                    id: "124".to_owned(),
                    provider: Some("tmdb".to_owned()),
                    identities,
                    name: "演员甲".to_owned(),
                    character: None,
                    order: Some(0),
                    profile_url: None,
                    person: None,
                }],
            )
            .await?;

        let relation_path =
            library_item_directory(config.path(), "item-multi-provider")?.join("people.json");
        let relation: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(relation_path).await?)?;
        let person_key = relation["actors"][0]["personKey"]
            .as_str()
            .ok_or("missing canonical person key")?;
        let person_dir = canonical_person_directory(config.path(), person_key)?;
        assert!(person_dir.join("person.nfo").exists());
        assert!(!config.path().join("metadata/people/演").exists());
        Ok(())
    }

    #[tokio::test]
    async fn nfo_relation_tracks_source_revision_and_reuses_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let first = [1_u8, 2, 3];
        let second = [4_u8, 5, 6];
        let actors = [ActorCredit {
            id: "9".to_owned(),
            provider: None,
            identities: Vec::new(),
            name: "演员甲".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: None,
        }];

        service
            .persist_nfo_item_actors("item-1", "tmdb", &actors, &first)
            .await?;
        let relation_path = library_item_directory(config.path(), "item-1")?.join("people.json");
        let relation: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(relation_path).await?)?;
        assert_eq!(relation["actors"][0]["pendingAssets"][0], "profileImage");
        assert!(
            service
                .item_actor_relation_is_current("item-1", &first)
                .await?
        );
        assert!(
            !service
                .item_actor_relation_is_current("item-1", &second)
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn nfo_relation_keeps_actor_when_person_assets_fail()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let person_dir = people_directory(config.path(), "演员甲", "tmdb", "9")?;
        tokio::fs::create_dir_all(person_dir.parent().ok_or("missing person parent")?).await?;
        tokio::fs::write(&person_dir, b"directory replacement").await?;

        let service = PeopleService::new(config.path().to_owned());
        let report = service
            .persist_nfo_item_actors(
                "item-1",
                "tmdb",
                &[ActorCredit {
                    id: "9".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: Some("角色甲".to_owned()),
                    order: Some(0),
                    profile_url: None,
                    person: None,
                }],
                &[1, 2, 3],
            )
            .await?;
        assert_eq!(report.stored_count, 1);
        assert!(!report.pending_assets.is_empty());

        let actors = service.list_item_actors("item-1").await?;
        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0].name, "演员甲");
        assert_eq!(actors[0].character.as_deref(), Some("角色甲"));
        Ok(())
    }
}
