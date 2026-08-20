use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fmt::Write as _,
    io::Cursor,
    path::{Component, Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use quick_xml::{
    escape::{escape, unescape},
    events::Event,
    reader::Reader,
};
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
use crate::storage::{
    Database, NewPersonCredit, PersonListOptions, StoredPersonCredit, StoredPersonIndexRebuildJob,
};

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
const PERSON_INDEX_REBUILD_SCHEMA_VERSION: i64 = 1;

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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_ids: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub production_locations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub premiere_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub production_year: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub taglines: Vec<String>,
}

impl PersonMetadata {
    fn supplement_missing_from(mut self, fallback: Self) -> Self {
        if self.biography.is_none() {
            self.biography = fallback.biography;
        }
        if self.birthday.is_none() {
            self.birthday = fallback.birthday;
        }
        if self.deathday.is_none() {
            self.deathday = fallback.deathday;
        }
        if self.known_for_department.is_none() {
            self.known_for_department = fallback.known_for_department;
        }
        if self.place_of_birth.is_none() {
            self.place_of_birth = fallback.place_of_birth;
        }
        if self.provider_ids.is_empty() {
            self.provider_ids = fallback.provider_ids;
        }
        if self.genres.is_empty() {
            self.genres = fallback.genres;
        }
        if self.tags.is_empty() {
            self.tags = fallback.tags;
        }
        if self.production_locations.is_empty() {
            self.production_locations = fallback.production_locations;
        }
        if self.premiere_date.is_none() {
            self.premiere_date = fallback.premiere_date;
        }
        if self.production_year.is_none() {
            self.production_year = fallback.production_year;
        }
        if self.taglines.is_empty() {
            self.taglines = fallback.taglines;
        }
        self
    }
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
    pub provider_ids: BTreeMap<String, String>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub production_locations: Vec<String>,
    pub premiere_date: Option<String>,
    pub production_year: Option<i32>,
    pub taglines: Vec<String>,
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
    pub provider_ids: BTreeMap<String, String>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub production_locations: Vec<String>,
    pub premiere_date: Option<String>,
    pub production_year: Option<i64>,
    pub taglines: Vec<String>,
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
        let nfo_result = async {
            let bytes = self
                .person_nfo_bytes_with_existing(
                    &nfo_path,
                    &actor.name,
                    provider,
                    provider_id,
                    actor.person.as_ref(),
                )
                .await?;
            write_atomically(&nfo_path, &bytes).await
        }
        .await;
        if let Err(error) = nfo_result {
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
                provider_ids: actor
                    .person
                    .as_ref()
                    .map(|person| person.provider_ids.clone())
                    .unwrap_or_default(),
                genres: actor
                    .person
                    .as_ref()
                    .map(|person| person.genres.clone())
                    .unwrap_or_default(),
                tags: actor
                    .person
                    .as_ref()
                    .map(|person| person.tags.clone())
                    .unwrap_or_default(),
                production_locations: actor
                    .person
                    .as_ref()
                    .map(|person| person.production_locations.clone())
                    .unwrap_or_default(),
                premiere_date: actor
                    .person
                    .as_ref()
                    .and_then(|person| person.premiere_date.clone()),
                production_year: actor
                    .person
                    .as_ref()
                    .and_then(|person| person.production_year.map(i64::from)),
                taglines: actor
                    .person
                    .as_ref()
                    .map(|person| person.taglines.clone())
                    .unwrap_or_default(),
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
        let jobs = database
            .sync_person_index_rebuild_jobs(PERSON_INDEX_REBUILD_SCHEMA_VERSION)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let mut rebuilt_items = 0;
        for job in jobs {
            if matches!(job.status.as_str(), "COMPLETED" | "CANCELLED") || job.cancel_requested {
                continue;
            }
            if !database
                .claim_person_index_rebuild_job(&job.library_id)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
            {
                continue;
            }
            match self.run_person_index_rebuild_job(database, &job).await {
                Ok(processed) => {
                    database
                        .finish_person_index_rebuild_job(&job.library_id, "COMPLETED", None)
                        .await
                        .map_err(|error| PeopleError::Storage(error.to_string()))?;
                    rebuilt_items += processed;
                }
                Err(error) => {
                    let detail = error.to_string();
                    let _ = database
                        .finish_person_index_rebuild_job(
                            &job.library_id,
                            "FAILED",
                            Some(detail.as_str()),
                        )
                        .await;
                    return Err(error);
                }
            }
        }
        Ok(rebuilt_items)
    }

    async fn run_person_index_rebuild_job(
        &self,
        database: &Database,
        job: &StoredPersonIndexRebuildJob,
    ) -> Result<usize, PeopleError> {
        let total_count = database
            .count_person_index_items(&job.library_id)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?;
        let total_count = (job.total_count > 0)
            .then_some(job.total_count)
            .unwrap_or(total_count);
        let mut after_id = job.cursor_id.clone();
        let mut processed_count = job.processed_count;
        loop {
            if database
                .person_index_rebuild_job_cancel_requested(&job.library_id)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?
            {
                break;
            }
            let item_ids = database
                .list_person_index_item_ids(
                    &job.library_id,
                    after_id.as_deref(),
                    PERSON_INDEX_REBUILD_BATCH_SIZE,
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
            if item_ids.is_empty() {
                break;
            }
            for item_id in &item_ids {
                match self.rebuild_item_person_credit_index(item_id).await {
                    Ok(()) => processed_count += 1,
                    Err(PeopleError::Serialization(message)) => {
                        tracing::warn!(item_id, %message, "skipping malformed people relation during index rebuild");
                        processed_count += 1;
                    }
                    Err(error) => return Err(error),
                }
                after_id = Some(item_id.clone());
            }
            if let Some(cursor_id) = after_id.as_deref() {
                database
                    .update_person_index_rebuild_progress(
                        &job.library_id,
                        cursor_id,
                        processed_count,
                        total_count,
                    )
                    .await
                    .map_err(|error| PeopleError::Storage(error.to_string()))?;
            }
            if item_ids.len() < PERSON_INDEX_REBUILD_BATCH_SIZE as usize {
                break;
            }
        }
        Ok(processed_count.saturating_sub(job.processed_count) as usize)
    }

    pub async fn cancel_person_index_rebuild(&self, library_id: &str) -> Result<bool, PeopleError> {
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
        let source_fingerprint = relation
            .as_ref()
            .and_then(|relation| relation.source_fingerprint.as_deref());
        if database
            .person_index_item_state_is_current(item_id, source_fingerprint)
            .await
            .map_err(|error| PeopleError::Storage(error.to_string()))?
        {
            return Ok(());
        }
        database
            .replace_person_credits_with_fingerprint(item_id, &credits, source_fingerprint)
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

    async fn update_person_metadata_with_nfo_mode(
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
                    provider_ids: update.provider_ids.clone(),
                    genres: update.genres.clone(),
                    tags: update.tags.clone(),
                    production_locations: update.production_locations.clone(),
                    premiere_date: update.premiere_date.clone(),
                    production_year: update.production_year,
                    taglines: update.taglines.clone(),
                });
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

    async fn write_person_nfo_for_actor(&self, actor: &StoredActor) -> Result<(), PeopleError> {
        self.write_person_nfo_for_actor_with_mode(actor, false)
            .await
    }

    async fn write_person_nfo_for_actor_replacing_metadata(
        &self,
        actor: &StoredActor,
    ) -> Result<(), PeopleError> {
        self.write_person_nfo_for_actor_with_mode(actor, true).await
    }

    async fn write_person_nfo_for_actor_with_mode(
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
        let legacy_dir = people_directory(&self.config_dir, &actor.name, provider, &person_id)
            .map_err(PeopleError::from)?;
        let person_dir = if safe_metadata(&legacy_dir).await.ok().flatten().is_some() {
            legacy_dir
        } else if let Some(person_key) = actor.person_key.as_deref() {
            canonical_person_directory(&self.config_dir, person_key).map_err(PeopleError::from)?
        } else {
            legacy_dir
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
            )
            .await?
        };
        write_atomically(&nfo_path, &bytes).await
    }

    async fn person_nfo_bytes_with_existing(
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
            merge_person_nfo_bytes(&existing, name, provider, provider_id, metadata)
                .unwrap_or(existing),
        )
    }

    async fn person_nfo_bytes_with_replaced_metadata(
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

    async fn actor_views_from_credits(&self, credits: Vec<StoredPersonCredit>) -> Vec<ActorView> {
        let mut views = Vec::with_capacity(credits.len());
        for credit in credits {
            let provider = (!credit.provider.is_empty()).then(|| credit.provider.clone());
            let image_url = self
                .person_image_url(provider.as_deref(), &credit.person_id)
                .await;
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
                provider,
                name: credit.person_name,
                character: (!credit.role.is_empty()).then_some(credit.role),
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

    async fn person_metadata_from_relation(
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
        person_name: &str,
        provider: Option<&str>,
        content_type: Option<&str>,
        bytes: &[u8],
    ) -> Result<(), PeopleError> {
        validate_component(person_id)?;
        if !is_valid_person_lookup(person_name) {
            return Err(PeopleError::InvalidComponent(person_name.to_owned()));
        }
        if bytes.is_empty() || bytes.len() > MAX_PROFILE_BYTES {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }
        let image_bytes = if profile_image_format(content_type, bytes).is_some() {
            bytes.to_owned()
        } else {
            BASE64.decode(bytes).map_err(|_| {
                PeopleError::InvalidImage("unsupported profile image type".to_owned())
            })?
        };
        let (extension, expected_type) = profile_image_format(content_type, &image_bytes)
            .ok_or_else(|| {
                PeopleError::InvalidImage("unsupported profile image type".to_owned())
            })?;
        if image_bytes.len() > MAX_PROFILE_BYTES || !valid_image(expected_type, &image_bytes) {
            return Err(PeopleError::InvalidImage(
                "profile image payload is invalid".to_owned(),
            ));
        }

        let provider = provider.unwrap_or("tmdb").trim().to_ascii_lowercase();
        validate_component(&provider)?;
        let person_dir = self
            .person_directory_for_update(person_name, &provider, person_id)
            .await?;
        create_private_dir(&person_dir).await?;
        let image_path = format!("{PERSON_IMAGE}.{extension}");
        write_atomically(&person_dir.join(&image_path), &image_bytes).await?;
        let relative_image_path = person_dir
            .join(&image_path)
            .strip_prefix(metadata_root(&self.config_dir))
            .map_err(|_| {
                PeopleError::Serialization("person image path is outside metadata".to_owned())
            })?
            .to_string_lossy()
            .into_owned();
        create_private_dir(&people_index_directory(&self.config_dir)).await?;
        let index = StoredPersonIndex {
            image_path: relative_image_path,
            person_key: person_key_for_identities(&[PersonIdentity {
                provider: provider.clone(),
                id: person_id.to_owned(),
            }]),
        };
        let index_bytes = serde_json::to_vec_pretty(&index)
            .map_err(|source| PeopleError::Serialization(source.to_string()))?;
        let provider_index_path =
            people_index_path_for_provider(&self.config_dir, &provider, person_id)
                .map_err(PeopleError::from)?;
        write_atomically(&provider_index_path, &index_bytes).await?;
        if provider.eq_ignore_ascii_case("tmdb") {
            let index_path =
                people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?;
            write_atomically(&index_path, &index_bytes).await?;
        }
        Ok(())
    }

    async fn person_directory_for_update(
        &self,
        person_name: &str,
        provider: &str,
        person_id: &str,
    ) -> Result<PathBuf, PeopleError> {
        let legacy_dir = people_directory(&self.config_dir, person_name, provider, person_id)
            .map_err(PeopleError::from)?;
        if safe_metadata(&legacy_dir).await?.is_some() {
            return Ok(legacy_dir);
        }

        let mut index_paths = vec![
            people_index_path_for_provider(&self.config_dir, provider, person_id)
                .map_err(PeopleError::from)?,
        ];
        if provider.eq_ignore_ascii_case("tmdb") {
            index_paths
                .push(people_index_path(&self.config_dir, person_id).map_err(PeopleError::from)?);
        }
        for index_path in index_paths {
            let Some(bytes) = read_people_file(&index_path).await? else {
                continue;
            };
            let index = serde_json::from_slice::<StoredPersonIndex>(&bytes)
                .map_err(|source| PeopleError::Serialization(source.to_string()))?;
            if let Some(person_key) = index.person_key.as_deref() {
                return canonical_person_directory(&self.config_dir, person_key)
                    .map_err(PeopleError::from);
            }
            let relative = Path::new(&index.image_path);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
                || relative.starts_with(Path::new("people/assets"))
            {
                continue;
            }
            let metadata_dir = metadata_root(&self.config_dir);
            let image_path = metadata_dir.join(relative);
            if image_path.starts_with(&metadata_dir)
                && let Some(parent) = image_path.parent()
                && safe_metadata(parent)
                    .await?
                    .is_some_and(|metadata| metadata.is_dir())
            {
                return Ok(parent.to_owned());
            }
        }

        let person_key = person_key_for_identities(&[PersonIdentity {
            provider: provider.to_owned(),
            id: person_id.to_owned(),
        }])
        .ok_or_else(|| PeopleError::InvalidComponent(person_id.to_owned()))?;
        canonical_person_directory(&self.config_dir, &person_key).map_err(PeopleError::from)
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
            if let Some(image) = self
                .indexed_profile_image_for_provider(provider, person_id)
                .await?
            {
                return Ok(Some(image));
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

    async fn indexed_profile_image_for_provider(
        &self,
        provider: &str,
        person_id: &str,
    ) -> Result<Option<PersonImage>, PeopleError> {
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
        if let Some(image) = self
            .indexed_profile_image_for_provider(provider, person_id)
            .await?
        {
            return self
                .materialize_profile_asset(&image.path, person_dir)
                .await
                .map(Some);
        }

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
        // downloading the same profile again. The shared bytes are
        // materialized as `folder.<ext>` in this person directory.
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
        let image_path = person_dir.join(format!("{PERSON_IMAGE}.{extension}"));
        write_atomically(&image_path, &bytes).await?;
        Ok(Some(format!("{PERSON_IMAGE}.{extension}")))
    }

    async fn materialize_profile_asset(
        &self,
        shared_path: &Path,
        person_dir: &Path,
    ) -> Result<String, PeopleError> {
        let Some(metadata) = safe_metadata(shared_path).await? else {
            return Err(PeopleError::Io {
                path: shared_path.to_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "shared profile asset does not exist",
                ),
            });
        };
        if !metadata.is_file() {
            return Err(PeopleError::Io {
                path: shared_path.to_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "shared profile asset is not a file",
                ),
            });
        }
        let extension = shared_path
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| PROFILE_EXTENSIONS.contains(value))
            .ok_or_else(|| {
                PeopleError::InvalidImage("shared profile asset extension is invalid".to_owned())
            })?;
        let file_name = format!("{PERSON_IMAGE}.{extension}");
        let target = person_dir.join(&file_name);
        let temporary = person_dir.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()));
        let result = async {
            // Materialize an old shared asset into the current Emby-compatible
            // folder.<ext> layout without copying its bytes.
            fs::hard_link(shared_path, &temporary)
                .await
                .map_err(|source| PeopleError::Io {
                    path: temporary.clone(),
                    source,
                })?;
            fs::rename(&temporary, &target)
                .await
                .map_err(|source| PeopleError::Io {
                    path: target.clone(),
                    source,
                })?;
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = fs::remove_file(&temporary).await;
        }
        result.map(|()| file_name)
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
    let metadata = metadata
        .map(|metadata| person_metadata_xml(metadata, provider, provider_id))
        .unwrap_or_default();
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

#[derive(Default)]
struct ParsedPersonNfo {
    fields: BTreeMap<String, String>,
    repeated_fields: BTreeMap<String, BTreeSet<String>>,
    uniqueids: BTreeSet<(String, String)>,
}

fn merge_person_nfo_bytes(
    existing: &[u8],
    name: &str,
    provider: &str,
    provider_id: &str,
    metadata: Option<&PersonMetadata>,
) -> Option<Vec<u8>> {
    let parsed = parse_person_nfo(existing)?;
    let mut additions = String::new();
    append_missing_person_nfo_field(&mut additions, &parsed, "name", Some(name));
    for (tag, value) in [
        (
            "biography",
            metadata.and_then(|value| value.biography.as_deref()),
        ),
        (
            "birthday",
            metadata.and_then(|value| value.birthday.as_deref()),
        ),
        (
            "deathday",
            metadata.and_then(|value| value.deathday.as_deref()),
        ),
        (
            "knownfor",
            metadata.and_then(|value| value.known_for_department.as_deref()),
        ),
        (
            "placeofbirth",
            metadata.and_then(|value| value.place_of_birth.as_deref()),
        ),
    ] {
        append_missing_person_nfo_field(&mut additions, &parsed, tag, value);
    }
    let mut known_uniqueids = parsed.uniqueids.clone();
    for (provider, provider_id) in metadata
        .into_iter()
        .flat_map(|metadata| metadata.provider_ids.iter())
    {
        append_missing_person_nfo_uniqueid(
            &mut additions,
            &mut known_uniqueids,
            provider,
            provider_id,
        );
    }
    let provider = provider.trim().to_ascii_lowercase();
    let provider_id = provider_id.trim();
    append_missing_person_nfo_uniqueid(
        &mut additions,
        &mut known_uniqueids,
        &provider,
        provider_id,
    );
    if let Some(metadata) = metadata {
        append_missing_person_nfo_values(&mut additions, &parsed, "genre", &metadata.genres);
        append_missing_person_nfo_values(&mut additions, &parsed, "tag", &metadata.tags);
        append_missing_person_nfo_values(
            &mut additions,
            &parsed,
            "country",
            &metadata.production_locations,
        );
        append_missing_person_nfo_values(&mut additions, &parsed, "tagline", &metadata.taglines);
        append_missing_person_nfo_field(
            &mut additions,
            &parsed,
            "premiered",
            metadata.premiere_date.as_deref(),
        );
        let production_year = metadata.production_year.map(|year| year.to_string());
        append_missing_person_nfo_field(
            &mut additions,
            &parsed,
            "year",
            production_year.as_deref(),
        );
    }
    if additions.is_empty() {
        return Some(existing.to_owned());
    }
    let mut existing = String::from_utf8(existing.to_owned()).ok()?;
    let closing = existing.rfind("</person>")?;
    existing.insert_str(closing, &additions);
    Some(existing.into_bytes())
}

fn replace_person_nfo_bytes(
    existing: &[u8],
    name: &str,
    provider: &str,
    provider_id: &str,
    metadata: Option<&PersonMetadata>,
) -> Option<Vec<u8>> {
    parse_person_nfo(existing)?;
    let mut document = String::from_utf8(existing.to_owned()).ok()?;
    for tag in [
        "name",
        "biography",
        "birthday",
        "deathday",
        "knownfor",
        "placeofbirth",
        "uniqueid",
        "genre",
        "tag",
        "country",
        "tagline",
        "premiered",
        "year",
    ] {
        remove_person_nfo_elements(&mut document, tag)?;
    }
    let closing = document.rfind("</person>")?;
    let generated =
        String::from_utf8(person_nfo_bytes(name, provider, provider_id, metadata)).ok()?;
    let generated_start = generated.find("<person>")? + "<person>".len();
    let generated_end = generated.rfind("</person>")?;
    document.insert_str(closing, &generated[generated_start..generated_end]);
    Some(document.into_bytes())
}

fn remove_person_nfo_elements(document: &mut String, tag: &str) -> Option<()> {
    let opening = format!("<{tag}");
    let closing = format!("</{tag}>");
    loop {
        let lower = document.to_ascii_lowercase();
        let Some(start) = find_person_nfo_element_start(&lower, &opening) else {
            return Some(());
        };
        let open_end = lower[start..].find('>')? + start;
        if lower.as_bytes().get(open_end.checked_sub(1)?) == Some(&b'/') {
            document.replace_range(start..=open_end, "");
            continue;
        }
        let content_start = open_end + 1;
        let close_start = lower[content_start..].find(&closing)? + content_start;
        let end = close_start + closing.len();
        document.replace_range(start..end, "");
    }
}

fn find_person_nfo_element_start(document: &str, opening: &str) -> Option<usize> {
    let mut search_from = 0;
    while let Some(relative_start) = document[search_from..].find(opening) {
        let start = search_from + relative_start;
        let boundary = document.as_bytes().get(start + opening.len()).copied();
        if matches!(
            boundary,
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            return Some(start);
        }
        search_from = start + opening.len();
    }
    None
}

fn append_missing_person_nfo_uniqueid(
    additions: &mut String,
    known_uniqueids: &mut BTreeSet<(String, String)>,
    provider: &str,
    provider_id: &str,
) {
    let provider = provider.trim().to_ascii_lowercase();
    let provider_id = provider_id.trim();
    if provider.is_empty()
        || provider_id.is_empty()
        || !known_uniqueids.insert((provider.clone(), provider_id.to_owned()))
    {
        return;
    }
    additions.push_str(&format!(
        "<uniqueid type=\"{}\">{}</uniqueid>",
        escape(&provider),
        escape(provider_id)
    ));
}

fn append_missing_person_nfo_field(
    additions: &mut String,
    parsed: &ParsedPersonNfo,
    tag: &str,
    value: Option<&str>,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    let already_present = parsed
        .fields
        .get(tag)
        .is_some_and(|value| !value.trim().is_empty());
    if !already_present {
        additions.push_str(&format!("<{tag}>{}</{tag}>", escape(value)));
    }
}

fn append_missing_person_nfo_values(
    additions: &mut String,
    parsed: &ParsedPersonNfo,
    tag: &str,
    values: &[String],
) {
    let existing = parsed.repeated_fields.get(tag);
    let mut appended = BTreeSet::new();
    for value in values
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if existing.is_some_and(|values| values.contains(value)) || !appended.insert(value) {
            continue;
        }
        additions.push_str(&format!("<{tag}>{}</{tag}>", escape(value)));
    }
}

fn parse_person_nfo(bytes: &[u8]) -> Option<ParsedPersonNfo> {
    if bytes.len() as u64 > MAX_PEOPLE_FILE_BYTES {
        return None;
    }
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut result = ParsedPersonNfo::default();
    let mut active: Option<(String, Option<String>, String)> = None;
    loop {
        match reader.read_event_into(&mut buffer).ok()? {
            Event::Eof => return Some(result),
            Event::Start(event) => {
                let tag = String::from_utf8(event.name().as_ref().to_ascii_lowercase()).ok()?;
                if tag == "uniqueid" {
                    let mut provider = None;
                    for attribute in event.attributes() {
                        let attribute = attribute.ok()?;
                        if attribute.key.as_ref() == b"type" {
                            provider = Some(attribute.unescape_value().ok()?.into_owned());
                        }
                    }
                    active = Some((tag, provider, String::new()));
                } else if matches!(
                    tag.as_str(),
                    "name"
                        | "biography"
                        | "birthday"
                        | "deathday"
                        | "knownfor"
                        | "placeofbirth"
                        | "premiered"
                        | "year"
                ) || matches!(tag.as_str(), "genre" | "tag" | "country" | "tagline")
                {
                    active = Some((tag, None, String::new()));
                }
            }
            Event::Text(text) => {
                if let Some((_, _, value)) = active.as_mut() {
                    let decoded = text.decode().ok()?;
                    value.push_str(unescape(decoded.as_ref()).ok()?.as_ref());
                }
            }
            Event::End(_) => {
                if let Some((tag, provider, value)) = active.take() {
                    let value = value.trim().to_owned();
                    if tag == "uniqueid" {
                        if let Some(provider) = provider
                            .map(|provider| provider.trim().to_ascii_lowercase())
                            .filter(|provider| !provider.is_empty())
                        {
                            if !value.is_empty() {
                                result.uniqueids.insert((provider, value));
                            }
                        }
                    } else if matches!(tag.as_str(), "genre" | "tag" | "country" | "tagline") {
                        result.repeated_fields.entry(tag).or_default().insert(value);
                    } else {
                        result.fields.entry(tag).or_insert(value);
                    }
                }
            }
            _ => {}
        }
        buffer.clear();
    }
}

fn person_metadata_xml(
    metadata: &PersonMetadata,
    primary_provider: &str,
    primary_id: &str,
) -> String {
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
    let mut uniqueids = BTreeSet::new();
    let primary_provider = primary_provider.trim().to_ascii_lowercase();
    let primary_id = primary_id.trim();
    if !primary_provider.is_empty() && !primary_id.is_empty() {
        uniqueids.insert((primary_provider, primary_id.to_owned()));
    }
    for (provider, provider_id) in &metadata.provider_ids {
        append_uniqueid_xml(&mut xml, &mut uniqueids, provider, provider_id);
    }
    for (tag, values) in [
        ("genre", &metadata.genres),
        ("tag", &metadata.tags),
        ("country", &metadata.production_locations),
        ("tagline", &metadata.taglines),
    ] {
        let mut seen = BTreeSet::new();
        for value in values
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !seen.insert(value) {
                continue;
            }
            xml.push_str(&format!("<{tag}>{}</{tag}>", escape(value)));
        }
    }
    if let Some(value) = metadata
        .premiere_date
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        xml.push_str(&format!("<premiered>{}</premiered>", escape(value)));
    }
    if let Some(value) = metadata.production_year {
        xml.push_str(&format!("<year>{value}</year>"));
    }
    xml
}

fn append_uniqueid_xml(
    xml: &mut String,
    known_uniqueids: &mut BTreeSet<(String, String)>,
    provider: &str,
    provider_id: &str,
) {
    let provider = provider.trim().to_ascii_lowercase();
    let provider_id = provider_id.trim();
    if provider.is_empty()
        || provider_id.is_empty()
        || !known_uniqueids.insert((provider.clone(), provider_id.to_owned()))
    {
        return;
    }
    xml.push_str(&format!(
        "<uniqueid type=\"{}\">{}</uniqueid>",
        escape(&provider),
        escape(provider_id)
    ));
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
        provider_ids: actor
            .person
            .as_ref()
            .map(|person| person.provider_ids.clone())
            .unwrap_or_default(),
        genres: actor
            .person
            .as_ref()
            .map(|person| person.genres.clone())
            .unwrap_or_default(),
        tags: actor
            .person
            .as_ref()
            .map(|person| person.tags.clone())
            .unwrap_or_default(),
        production_locations: actor
            .person
            .as_ref()
            .map(|person| person.production_locations.clone())
            .unwrap_or_default(),
        premiere_date: actor
            .person
            .as_ref()
            .and_then(|person| person.premiere_date.clone()),
        production_year: actor
            .person
            .as_ref()
            .and_then(|person| person.production_year.map(i64::from)),
        taglines: actor
            .person
            .as_ref()
            .map(|person| person.taglines.clone())
            .unwrap_or_default(),
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
    let detected = detected_profile_image_format(bytes);
    match content_type.as_deref() {
        Some("image/jpeg") | Some("image/jpg") | Some("image/png") | Some("image/webp") => detected,
        Some(_) => None,
        None => detected,
    }
}

fn detected_profile_image_format(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if valid_image("image/jpeg", bytes) {
        Some(("jpg", "image/jpeg"))
    } else if valid_image("image/png", bytes) {
        Some(("png", "image/png"))
    } else if valid_image("image/webp", bytes) {
        Some(("webp", "image/webp"))
    } else {
        None
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;

    use super::{ActorCredit, PeopleError, PeopleService, PersonIdentity};
    use crate::application::metadata_paths::{
        canonical_person_directory, library_item_directory, people_directory,
        people_index_path_for_provider,
    };

    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

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
                        provider_ids: BTreeMap::new(),
                        genres: Vec::new(),
                        tags: Vec::new(),
                        production_locations: Vec::new(),
                        premiere_date: None,
                        production_year: None,
                        taglines: Vec::new(),
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
    async fn local_person_metadata_nfo_uses_local_identity_type()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let actor = super::StoredActor {
            id: Some("local-test".to_owned()),
            name: "本地演员".to_owned(),
            provider: String::new(),
            person_key: None,
            identities: Vec::new(),
            character: None,
            order: Some(0),
            image_file: None,
            pending_assets: Vec::new(),
            person: Some(super::PersonMetadata {
                biography: Some("本地演员简介".to_owned()),
                birthday: None,
                deathday: None,
                known_for_department: None,
                place_of_birth: None,
                provider_ids: BTreeMap::new(),
                genres: Vec::new(),
                tags: Vec::new(),
                production_locations: Vec::new(),
                premiere_date: None,
                production_year: None,
                taglines: Vec::new(),
            }),
        };

        service.write_person_nfo_for_actor(&actor).await?;

        let nfo_path = people_directory(config.path(), "本地演员", "local", "local-test")?
            .join(super::PERSON_NFO);
        let nfo = tokio::fs::read_to_string(nfo_path).await?;
        assert!(nfo.contains("<uniqueid type=\"local\">local-test</uniqueid>"));
        assert!(nfo.contains("<biography>本地演员简介</biography>"));
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
    async fn legacy_shared_profile_asset_is_materialized_in_person_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::MetadataExt;

        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let shared_relative = "people/assets/legacy-hash.png";
        let shared_path = config.path().join("metadata").join(shared_relative);
        tokio::fs::create_dir_all(shared_path.parent().ok_or("missing shared parent")?).await?;
        tokio::fs::write(&shared_path, b"same-image").await?;
        let person_key = super::person_key_for_identities(&[PersonIdentity {
            provider: "tmdb".to_owned(),
            id: "9".to_owned(),
        }])
        .ok_or("missing person key")?;
        let index_path = people_index_path_for_provider(config.path(), "tmdb", "9")?;
        tokio::fs::create_dir_all(index_path.parent().ok_or("missing index parent")?).await?;
        tokio::fs::write(
            &index_path,
            serde_json::to_vec(&serde_json::json!({
                "imagePath": shared_relative,
                "personKey": person_key,
            }))?,
        )
        .await?;

        service
            .persist_item_actors(
                "item-shared-profile",
                "tmdb",
                &[ActorCredit {
                    id: "9".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: None,
                    order: Some(0),
                    profile_url: None,
                    person: None,
                }],
            )
            .await?;

        let relation_path =
            library_item_directory(config.path(), "item-shared-profile")?.join("people.json");
        let relation: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(relation_path).await?)?;
        assert_eq!(relation["actors"][0]["imageFile"], "folder.png");

        let person_dir = canonical_person_directory(config.path(), &person_key)?;
        let person_image = person_dir.join("folder.png");
        assert_eq!(tokio::fs::read(&person_image).await?, b"same-image");
        assert_eq!(
            tokio::fs::metadata(&person_image).await?.ino(),
            tokio::fs::metadata(shared_path).await?.ino()
        );
        Ok(())
    }

    #[tokio::test]
    async fn uploaded_profile_image_is_written_in_person_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        service
            .update_person_image("9", "演员甲", Some("tmdb"), Some("image/png"), PNG_1X1)
            .await?;

        let person_key = super::person_key_for_identities(&[PersonIdentity {
            provider: "tmdb".to_owned(),
            id: "9".to_owned(),
        }])
        .ok_or("missing person key")?;
        let person_dir = canonical_person_directory(config.path(), &person_key)?;
        let person_image = person_dir.join("folder.png");
        assert_eq!(tokio::fs::read(&person_image).await?, PNG_1X1);
        assert!(!config.path().join("metadata/people/assets").exists());
        let index_path = people_index_path_for_provider(config.path(), "tmdb", "9")?;
        let index: serde_json::Value = serde_json::from_slice(&tokio::fs::read(index_path).await?)?;
        assert!(
            index["imagePath"]
                .as_str()
                .is_some_and(|path| path.ends_with("/folder.png"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn person_nfo_supplements_missing_fields_without_replacing_existing_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let identities = vec![
            PersonIdentity {
                provider: "tmdb".to_owned(),
                id: "9".to_owned(),
            },
            PersonIdentity {
                provider: "douban".to_owned(),
                id: "db-9".to_owned(),
            },
        ];
        service
            .persist_item_actors(
                "item-nfo-first",
                "tmdb",
                &[ActorCredit {
                    id: "9".to_owned(),
                    provider: Some("tmdb".to_owned()),
                    identities: identities.clone(),
                    name: "演员甲".to_owned(),
                    character: None,
                    order: Some(0),
                    profile_url: None,
                    person: Some(super::PersonMetadata {
                        biography: Some("TMDb biography".to_owned()),
                        birthday: None,
                        deathday: None,
                        known_for_department: None,
                        place_of_birth: None,
                        provider_ids: BTreeMap::new(),
                        genres: Vec::new(),
                        tags: Vec::new(),
                        production_locations: Vec::new(),
                        premiere_date: None,
                        production_year: None,
                        taglines: Vec::new(),
                    }),
                }],
            )
            .await?;
        service
            .persist_item_actors(
                "item-nfo-second",
                "douban",
                &[ActorCredit {
                    id: "db-9".to_owned(),
                    provider: Some("douban".to_owned()),
                    identities,
                    name: "演员甲".to_owned(),
                    character: None,
                    order: Some(0),
                    profile_url: None,
                    person: Some(super::PersonMetadata {
                        biography: Some("Douban biography".to_owned()),
                        birthday: Some("1970-01-01".to_owned()),
                        deathday: None,
                        known_for_department: None,
                        place_of_birth: None,
                        provider_ids: BTreeMap::new(),
                        genres: Vec::new(),
                        tags: Vec::new(),
                        production_locations: Vec::new(),
                        premiere_date: None,
                        production_year: None,
                        taglines: Vec::new(),
                    }),
                }],
            )
            .await?;

        let person_key = super::person_key_for_identities(&[
            PersonIdentity {
                provider: "douban".to_owned(),
                id: "db-9".to_owned(),
            },
            PersonIdentity {
                provider: "tmdb".to_owned(),
                id: "9".to_owned(),
            },
        ])
        .ok_or("missing person key")?;
        let nfo_path = canonical_person_directory(config.path(), &person_key)?.join("person.nfo");
        let nfo = tokio::fs::read_to_string(nfo_path).await?;
        assert!(nfo.contains("<biography>TMDb biography</biography>"));
        assert!(!nfo.contains("Douban biography"));
        assert!(nfo.contains("<birthday>1970-01-01</birthday>"));
        Ok(())
    }

    #[test]
    fn person_nfo_appends_mdc_fields_without_duplicates_and_escapes_values() {
        let existing = "<?xml version=\"1.0\"?><person><name>旧姓名</name><genre>已有类型</genre><uniqueid type=\"tmdb\">9</uniqueid><custom>保留</custom></person>".as_bytes();
        let mut provider_ids = BTreeMap::new();
        provider_ids.insert("Tmdb".to_owned(), "9".to_owned());
        provider_ids.insert("Imdb".to_owned(), "nm<&>".to_owned());
        let metadata = super::PersonMetadata {
            biography: None,
            birthday: None,
            deathday: None,
            known_for_department: None,
            place_of_birth: None,
            provider_ids,
            genres: vec![
                "已有类型".to_owned(),
                "新 & 类型".to_owned(),
                "新 & 类型".to_owned(),
            ],
            tags: vec!["MDC".to_owned(), "MDC".to_owned()],
            production_locations: vec!["日本".to_owned()],
            premiere_date: Some("2000-01-02".to_owned()),
            production_year: Some(2000),
            taglines: vec!["A <tagline>".to_owned()],
        };

        let nfo = String::from_utf8(
            super::merge_person_nfo_bytes(existing, "新姓名", "tmdb", "9", Some(&metadata))
                .expect("valid person nfo"),
        )
        .expect("utf-8 nfo");

        assert!(nfo.contains("<name>旧姓名</name>"));
        assert!(nfo.contains("<custom>保留</custom>"));
        assert_eq!(
            nfo.matches("<uniqueid type=\"tmdb\">9</uniqueid>").count(),
            1
        );
        assert!(nfo.contains("<uniqueid type=\"imdb\">nm&lt;&amp;&gt;</uniqueid>"));
        assert_eq!(nfo.matches("<genre>已有类型</genre>").count(), 1);
        assert!(nfo.contains("<genre>新 &amp; 类型</genre>"));
        assert_eq!(nfo.matches("<tag>MDC</tag>").count(), 1);
        assert!(nfo.contains("<country>日本</country>"));
        assert!(nfo.contains("<premiered>2000-01-02</premiered>"));
        assert!(nfo.contains("<year>2000</year>"));
        assert!(nfo.contains("<tagline>A &lt;tagline&gt;</tagline>"));
    }

    #[test]
    fn person_nfo_replacement_updates_known_fields_and_preserves_unknown_xml() {
        let existing = "<?xml version=\"1.0\"?><person><name>旧姓名</name><biography>旧简介</biography><genre>旧类型</genre><uniqueid type=\"imdb\">old-id</uniqueid><custom>保留</custom></person>".as_bytes();
        let metadata = super::PersonMetadata {
            biography: Some("新简介".to_owned()),
            birthday: None,
            deathday: None,
            known_for_department: None,
            place_of_birth: None,
            provider_ids: BTreeMap::new(),
            genres: vec!["新类型".to_owned()],
            tags: Vec::new(),
            production_locations: Vec::new(),
            premiere_date: None,
            production_year: None,
            taglines: Vec::new(),
        };

        let nfo = String::from_utf8(
            super::replace_person_nfo_bytes(
                existing,
                "新姓名",
                "local",
                "local-id",
                Some(&metadata),
            )
            .expect("valid person nfo"),
        )
        .expect("utf-8 nfo");

        assert!(nfo.contains("<name>新姓名</name>"));
        assert!(!nfo.contains("旧姓名"));
        assert!(nfo.contains("<biography>新简介</biography>"));
        assert!(!nfo.contains("旧简介"));
        assert!(nfo.contains("<genre>新类型</genre>"));
        assert!(!nfo.contains("旧类型"));
        assert!(nfo.contains("<uniqueid type=\"local\">local-id</uniqueid>"));
        assert!(!nfo.contains("old-id"));
        assert!(nfo.contains("<custom>保留</custom>"));
    }

    #[tokio::test]
    async fn uploaded_profile_index_wins_over_an_older_folder_variant()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let legacy_dir = people_directory(config.path(), "演员甲", "tmdb", "9")?;
        tokio::fs::create_dir_all(&legacy_dir).await?;
        tokio::fs::write(legacy_dir.join("folder.jpg"), b"old-image").await?;
        let service = PeopleService::new(config.path().to_owned());
        service
            .update_person_image("9", "演员甲", Some("tmdb"), Some("image/png"), PNG_1X1)
            .await?;
        service
            .persist_item_actors(
                "item-uploaded-profile",
                "tmdb",
                &[ActorCredit {
                    id: "9".to_owned(),
                    provider: None,
                    identities: Vec::new(),
                    name: "演员甲".to_owned(),
                    character: None,
                    order: Some(0),
                    profile_url: None,
                    person: None,
                }],
            )
            .await?;

        let relation_path =
            library_item_directory(config.path(), "item-uploaded-profile")?.join("people.json");
        let relation: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(relation_path).await?)?;
        assert_eq!(relation["actors"][0]["imageFile"], "folder.png");
        assert_eq!(
            tokio::fs::read(legacy_dir.join("folder.png")).await?,
            PNG_1X1
        );
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
