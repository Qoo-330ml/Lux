use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    fmt::Write as _,
    io::Cursor,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

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
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, OnceCell, Semaphore},
    time::{Duration, sleep},
};
use uuid::Uuid;

use crate::application::metadata_paths::{
    MetadataPathError, canonical_person_directory, library_item_directory, lux_person_directory,
    metadata_root, people_directory, people_index_directory, people_index_path,
    people_index_path_for_provider, readable_component,
};
use crate::application::remote_body::{LimitedBodyError, read_response_body_limited};
use crate::storage::{
    Database, NewPersonCredit, PersonListOptions, PersonMatchCandidateRestore, StoredPersonCredit,
    StoredPersonIndexRebuildJob, StoredPersonMatchCandidate,
};

#[path = "helpers.rs"]
mod helpers;
#[allow(unused_imports)]
use helpers::*;
#[path = "assets.rs"]
mod assets;
#[path = "matching.rs"]
mod matching;
#[path = "metadata.rs"]
mod metadata;
#[path = "rebuild.rs"]
mod rebuild;
#[path = "relations.rs"]
mod relations;

const LEGACY_PEOPLE_DIR: &str = "people";
const LEGACY_ITEMS_DIR: &str = "items";
const LEGACY_PROFILES_DIR: &str = "profiles";
const PERSON_NFO: &str = "person.nfo";
const PERSON_MANIFEST: &str = "person.json";
const PERSON_IMAGE: &str = "folder";
const PEOPLE_RELATION_SCHEMA_VERSION: u32 = 4;
const PERSON_MANIFEST_SCHEMA_VERSION: u32 = 3;
const LEGACY_PERSON_MIGRATION_SCHEMA_VERSION: i64 = 1;
const PENDING_PERSON_DIRECTORY: &str = "personDirectory";
const PENDING_PERSON_NFO: &str = "personNfo";
const PENDING_PERSON_MANIFEST: &str = "personManifest";
const PENDING_PROFILE_IMAGE: &str = "profileImage";
const PENDING_PERSON_INDEX: &str = "personIndex";
const PERSON_MATCH_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const PERSON_MATCH_SNAPSHOT_DIR: &str = "matches";
const PEOPLE_RELATION_QUARANTINE_DIR: &str = "people-relations";
const PERSON_DECISION_OPERATION_SCHEMA_VERSION: u32 = 1;
const PERSON_DECISION_OPERATION_DIR: &str = "operations";
const MAX_ACTORS: usize = 100;
const MAX_PEOPLE_FILE_BYTES: u64 = 256 * 1024;
const MAX_PROFILE_BYTES: usize = 10 * 1024 * 1024;
const PROFILE_EXTENSIONS: [&str; 3] = ["jpg", "png", "webp"];
const PERSON_INDEX_REBUILD_BATCH_SIZE: i64 = 100;
const PERSON_INDEX_REBUILD_SCHEMA_VERSION: i64 = 1;
const PERSON_LOCKABLE_FIELDS: [&str; 14] = [
    "name",
    "biography",
    "birthday",
    "deathday",
    "knownForDepartment",
    "placeOfBirth",
    "providerIds",
    "genres",
    "tags",
    "productionLocations",
    "premiereDate",
    "productionYear",
    "taglines",
    "aliases",
];
const MAX_LOCAL_LEGACY_RELATION_DIRECTORY_ENTRIES: usize = 4096;
const LOCAL_NFO_PERSON_ASSET_BATCH_CONCURRENCY: usize = 4;

#[derive(Clone, Default)]
pub(crate) struct LocalActorRelationPageCache {
    relevant_item_ids: Arc<HashSet<String>>,
    legacy_item_ids: Arc<OnceCell<Result<LegacyItemDirectorySnapshot, String>>>,
}

impl LocalActorRelationPageCache {
    pub(crate) fn new(item_ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            relevant_item_ids: Arc::new(item_ids.into_iter().collect()),
            legacy_item_ids: Arc::new(OnceCell::new()),
        }
    }
}

struct LegacyItemDirectorySnapshot {
    item_ids: HashSet<String>,
    complete: bool,
}

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

fn metadata_update_respecting_locks(
    existing: &PersonMetadata,
    update: &PersonMetadataUpdate,
    locked_fields: &BTreeSet<String>,
) -> PersonMetadata {
    let locked = |field: &str| locked_fields.contains(field);
    PersonMetadata {
        biography: (locked("biography") && existing.biography.is_some())
            .then(|| existing.biography.clone())
            .flatten()
            .or_else(|| update.biography.clone()),
        birthday: (locked("birthday") && existing.birthday.is_some())
            .then(|| existing.birthday.clone())
            .flatten()
            .or_else(|| update.birthday.clone()),
        deathday: (locked("deathday") && existing.deathday.is_some())
            .then(|| existing.deathday.clone())
            .flatten()
            .or_else(|| update.deathday.clone()),
        known_for_department: (locked("knownForDepartment")
            && existing.known_for_department.is_some())
        .then(|| existing.known_for_department.clone())
        .flatten()
        .or_else(|| update.known_for_department.clone()),
        place_of_birth: (locked("placeOfBirth") && existing.place_of_birth.is_some())
            .then(|| existing.place_of_birth.clone())
            .flatten()
            .or_else(|| update.place_of_birth.clone()),
        provider_ids: if locked("providerIds") && !existing.provider_ids.is_empty() {
            existing.provider_ids.clone()
        } else {
            update.provider_ids.clone()
        },
        genres: if locked("genres") && !existing.genres.is_empty() {
            existing.genres.clone()
        } else {
            update.genres.clone()
        },
        tags: if locked("tags") && !existing.tags.is_empty() {
            existing.tags.clone()
        } else {
            update.tags.clone()
        },
        production_locations: if locked("productionLocations")
            && !existing.production_locations.is_empty()
        {
            existing.production_locations.clone()
        } else {
            update.production_locations.clone()
        },
        premiere_date: (locked("premiereDate") && existing.premiere_date.is_some())
            .then(|| existing.premiere_date.clone())
            .flatten()
            .or_else(|| update.premiere_date.clone()),
        production_year: (locked("productionYear") && existing.production_year.is_some())
            .then_some(existing.production_year)
            .flatten()
            .or(update.production_year),
        taglines: if locked("taglines") && !existing.taglines.is_empty() {
            existing.taglines.clone()
        } else {
            update.taglines.clone()
        },
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lux_person_id: Option<String>,
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
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_relative_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_modified_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_production_year: Option<i32>,
    #[serde(default)]
    actors: Vec<StoredActor>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActorPersistReport {
    pub stored_count: usize,
    pub pending_assets: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct DeferredNfoActorCredits {
    pending: Arc<AsyncMutex<Vec<PendingNfoActorCredits>>>,
    flush_lock: Arc<AsyncMutex<()>>,
    manifest_restore_pending: Arc<AsyncMutex<bool>>,
    person_asset_results: Arc<AsyncMutex<HashMap<String, Arc<OnceCell<PersonAssetResult>>>>>,
    person_asset_permits: Arc<Semaphore>,
    #[cfg(test)]
    person_asset_test_probe: Option<LocalNfoPersonAssetConcurrencyProbe>,
}

#[cfg(test)]
#[derive(Clone)]
struct LocalNfoPersonAssetConcurrencyProbe {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

#[cfg(test)]
impl LocalNfoPersonAssetConcurrencyProbe {
    async fn hold_after_permit(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _ = self.started.send(());
        if let Ok(permit) = self.release.acquire().await {
            drop(permit);
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Default for DeferredNfoActorCredits {
    fn default() -> Self {
        Self {
            pending: Arc::default(),
            manifest_restore_pending: Arc::default(),
            person_asset_results: Arc::default(),
            person_asset_permits: Arc::new(Semaphore::new(
                LOCAL_NFO_PERSON_ASSET_BATCH_CONCURRENCY,
            )),
            #[cfg(test)]
            person_asset_test_probe: None,
        }
    }
}

pub(super) struct PersonManifestWriteOptions<'a> {
    pub metadata: Option<&'a PersonMetadata>,
    pub deferred_restore_pending: Option<&'a DeferredNfoActorCredits>,
}

#[derive(Clone)]
struct PendingNfoActorCredits {
    item_id: String,
    credits: Vec<NewPersonCredit>,
    source_fingerprint: Option<String>,
    relation_checksum: String,
}

pub(crate) struct NfoActorCreditsFlushFailure {
    pub item_ids: Vec<String>,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonMatchCandidateView {
    pub id: String,
    pub item_id: String,
    pub provider: String,
    pub provider_id: String,
    pub candidate_person_ids: Vec<String>,
    pub status: String,
    pub score: Option<f64>,
    pub evidence: Value,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonIdentityMove {
    pub previous_person_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonMatchCandidateSnapshot {
    schema_version: u32,
    id: String,
    item_id: String,
    provider: String,
    provider_id: String,
    candidate_person_ids: Vec<String>,
    status: String,
    score: Option<f64>,
    evidence: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_person_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_person_id: Option<String>,
    created_at: i64,
    updated_at: i64,
    #[serde(default)]
    checksum: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonDecisionOperation {
    schema_version: u32,
    operation_id: String,
    operation: String,
    candidate_id: String,
    item_id: String,
    candidate_person_ids_json: String,
    score: Option<f64>,
    provider: String,
    provider_id: String,
    target_person_id: String,
    previous_person_id: Option<String>,
    state: String,
    evidence_json: String,
    created_at: i64,
    updated_at: i64,
    #[serde(default)]
    checksum: String,
}

#[derive(Clone, Default)]
struct PersonAssetResult {
    image_file: Option<String>,
    pending_assets: Vec<String>,
}

impl PersonAssetResult {
    fn failed() -> Self {
        Self {
            image_file: None,
            pending_assets: vec![
                PENDING_PERSON_DIRECTORY.to_owned(),
                PENDING_PERSON_NFO.to_owned(),
                PENDING_PERSON_MANIFEST.to_owned(),
                PENDING_PROFILE_IMAGE.to_owned(),
                PENDING_PERSON_INDEX.to_owned(),
            ],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredPersonIndex {
    image_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    person_key: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonManifest {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    generation: u64,
    lux_person_id: String,
    display_name: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    aliases: BTreeSet<String>,
    identities: Vec<PersonIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    person: Option<PersonMetadata>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    field_sources: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    locked_fields: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    identity_events: Vec<PersonManifestIdentityEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    metadata_events: Vec<PersonManifestMetadataEvent>,
    checksum: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonManifestIdentityEvent {
    event_id: String,
    event_type: String,
    provider: String,
    provider_id: String,
    from_person_id: Option<String>,
    to_person_id: Option<String>,
    evidence_json: String,
    created_at: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonManifestMetadataEvent {
    event_id: String,
    event_type: String,
    fields: Vec<String>,
    evidence_json: String,
    created_at: i64,
}

#[derive(Default)]
struct PersonManifestRestoreReport {
    restored: usize,
    failed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorView {
    pub id: String,
    #[serde(skip)]
    pub(crate) lookup_id: String,
    pub provider: Option<String>,
    pub name: String,
    pub character: Option<String>,
    pub is_favorite: bool,
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
    rebuild_lock: Arc<AsyncMutex<()>>,
    person_asset_locks: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    relation_locks: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    rebuild_coordinator: PersonIndexRebuildCoordinator,
}

#[derive(Clone, Default)]
struct PersonIndexRebuildCoordinator {
    state: Arc<AsyncMutex<PersonIndexRebuildCoordinatorState>>,
}

#[derive(Default)]
struct PersonIndexRebuildCoordinatorState {
    running: bool,
    pending: bool,
}

impl PersonIndexRebuildCoordinator {
    async fn begin(&self) -> bool {
        let mut state = self.state.lock().await;
        if state.running {
            state.pending = true;
            return false;
        }
        state.running = true;
        true
    }

    async fn finish(&self) -> bool {
        let mut state = self.state.lock().await;
        if state.pending {
            state.pending = false;
            true
        } else {
            state.running = false;
            false
        }
    }
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

    async fn mark_person_manifest_restore_pending(&self) -> Result<(), PeopleError> {
        if let Some(database) = &self.database {
            database
                .mark_person_manifest_restore_pending(PERSON_MANIFEST_SCHEMA_VERSION as i64)
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()))?;
        }
        Ok(())
    }

    async fn ensure_person_manifest_restore_pending(
        &self,
        deferred: Option<&DeferredNfoActorCredits>,
    ) -> Result<(), PeopleError> {
        if let Some(deferred) = deferred {
            let mut marked = deferred.manifest_restore_pending.lock().await;
            if !*marked {
                self.mark_person_manifest_restore_pending().await?;
                *marked = true;
            }
            Ok(())
        } else {
            self.mark_person_manifest_restore_pending().await
        }
    }

    pub(super) async fn relation_lock_for(&self, relation_path: &Path) -> Arc<AsyncMutex<()>> {
        let key = relation_path.to_string_lossy().into_owned();
        let mut locks = self.relation_locks.lock().await;
        locks
            .entry(key)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
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
            rebuild_lock: Arc::new(AsyncMutex::new(())),
            person_asset_locks: Arc::new(AsyncMutex::new(HashMap::new())),
            relation_locks: Arc::new(AsyncMutex::new(HashMap::new())),
            rebuild_coordinator: PersonIndexRebuildCoordinator::default(),
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

    pub(crate) async fn item_actor_relation_exists_with_page_cache(
        &self,
        item_id: &str,
        cache: &LocalActorRelationPageCache,
    ) -> Result<(bool, bool), PeopleError> {
        let new_path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        if read_relation(&new_path).await?.is_some() {
            return Ok((true, false));
        }

        let mut loaded = false;
        let legacy_item_ids = cache
            .legacy_item_ids
            .get_or_init(|| async {
                loaded = true;
                self.read_legacy_item_relation_ids(&cache.relevant_item_ids)
                    .await
            })
            .await;
        let legacy_path = self
            .legacy_people_dir()
            .join(LEGACY_ITEMS_DIR)
            .join(format!("{item_id}.json"));
        match legacy_item_ids {
            Ok(snapshot) if snapshot.item_ids.contains(item_id) => {
                Ok((read_relation(&legacy_path).await?.is_some(), loaded))
            }
            Ok(snapshot) if snapshot.complete => Ok((false, loaded)),
            Ok(_) => Ok((read_relation(&legacy_path).await?.is_some(), loaded)),
            Err(_directory_error) => Ok((read_relation(&legacy_path).await?.is_some(), loaded)),
        }
    }

    async fn read_legacy_item_relation_ids(
        &self,
        relevant_item_ids: &HashSet<String>,
    ) -> Result<LegacyItemDirectorySnapshot, String> {
        let directory = self.legacy_people_dir().join(LEGACY_ITEMS_DIR);
        let mut entries = match fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LegacyItemDirectorySnapshot {
                    item_ids: HashSet::new(),
                    complete: true,
                });
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut item_ids = HashSet::new();
        let mut entry_count = 0;
        let mut complete = true;
        loop {
            if entry_count >= MAX_LOCAL_LEGACY_RELATION_DIRECTORY_ENTRIES {
                complete = false;
                break;
            }
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => return Err(error.to_string()),
            };
            entry_count += 1;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Some(item_id) = name.strip_suffix(".json") {
                if relevant_item_ids.contains(item_id) {
                    item_ids.insert(item_id.to_owned());
                    if item_ids.len() == relevant_item_ids.len() {
                        complete = false;
                        break;
                    }
                }
            }
        }
        Ok(LegacyItemDirectorySnapshot { item_ids, complete })
    }

    pub async fn item_actor_relation_is_current(
        &self,
        item_id: &str,
        source_fingerprint: &[u8],
    ) -> Result<bool, PeopleError> {
        let path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let Some(relation_bytes) = read_people_file(&path).await? else {
            return Ok(false);
        };
        let relation_checksum = relation_snapshot_checksum(&relation_bytes);
        let relation = parse_relation(&relation_bytes)?;
        let relation_is_current = relation
            .source_fingerprint
            .as_deref()
            .and_then(decode_fingerprint)
            .is_some_and(|stored| stored == source_fingerprint);
        if !relation_is_current {
            return Ok(false);
        }
        if let Some(database) = &self.database {
            return database
                .person_index_item_state_matches_snapshot(
                    item_id,
                    relation.source_fingerprint.as_deref(),
                    Some(&relation_checksum),
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()));
        }
        Ok(true)
    }

    pub async fn nfo_relation_snapshot_is_current(
        &self,
        item_id: &str,
    ) -> Result<bool, PeopleError> {
        let path = library_item_directory(&self.config_dir, item_id)
            .map_err(PeopleError::from)?
            .join("people.json");
        let Some(relation_bytes) = read_people_file(&path).await? else {
            return Ok(false);
        };
        let relation_checksum = relation_snapshot_checksum(&relation_bytes);
        let relation = parse_relation(&relation_bytes)?;
        let source_fingerprint = relation
            .source_fingerprint
            .as_deref()
            .and_then(decode_fingerprint)
            .filter(|fingerprint| fingerprint.len() == 32);
        let Some(source_fingerprint) = source_fingerprint else {
            return Ok(false);
        };
        if let Some(database) = &self.database {
            return database
                .person_index_item_state_matches_snapshot(
                    item_id,
                    relation.source_fingerprint.as_deref(),
                    Some(&relation_checksum),
                )
                .await
                .map_err(|error| PeopleError::Storage(error.to_string()));
        }
        Ok(source_fingerprint.len() == 32)
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
            let lookup_id = actor.lux_person_id.clone().unwrap_or_else(|| id.clone());
            let provider = actor_provider_from_stored_actor(&actor);
            let image_url = self.person_image_url(provider.as_deref(), &id).await;
            views.push(ActorView {
                id,
                lookup_id,
                provider,
                name: actor.name,
                character: actor.character,
                is_favorite: false,
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
    use std::{
        collections::{BTreeMap, BTreeSet, HashSet},
        path::Path,
        sync::Arc,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::{Mutex as AsyncMutex, Semaphore};

    use super::{
        ActorCredit, DeferredNfoActorCredits, LOCAL_NFO_PERSON_ASSET_BATCH_CONCURRENCY,
        LocalActorRelationPageCache, LocalNfoPersonAssetConcurrencyProbe,
        MAX_LOCAL_LEGACY_RELATION_DIRECTORY_ENTRIES, PENDING_PERSON_INDEX, PENDING_PERSON_MANIFEST,
        PERSON_MANIFEST, PERSON_MANIFEST_SCHEMA_VERSION, PERSON_NFO, PeopleError, PeopleService,
        PersonIdentity, PersonIndexRebuildCoordinator, PersonManifest, PersonManifestWriteOptions,
        PersonMetadata, PersonMetadataUpdate, relation_snapshot_checksum,
        write_atomically_if_changed,
    };
    use crate::application::metadata_paths::{
        canonical_person_directory, library_item_directory, lux_person_directory, metadata_root,
        people_directory, people_index_path, people_index_path_for_provider,
    };
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };
    use sha2::{Digest, Sha256};

    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    async fn assert_relation_checksum_matches_file(
        database: &Database,
        config_dir: &Path,
        item_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let relation_bytes =
            tokio::fs::read(library_item_directory(config_dir, item_id)?.join("people.json"))
                .await?;
        let expected_checksum = Sha256::digest(&relation_bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let stored_checksum: Option<String> = sqlx::query_scalar(
            "SELECT relation_checksum FROM person_index_item_state WHERE item_id = ?",
        )
        .bind(item_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(
            stored_checksum.as_deref(),
            Some(expected_checksum.as_str()),
            "the index state must checksum the exact bytes written to people.json for {item_id}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn person_index_rebuild_requests_coalesce_while_a_run_is_active() {
        let coordinator = PersonIndexRebuildCoordinator::default();

        assert!(coordinator.begin().await);
        assert!(!coordinator.begin().await);
        assert!(coordinator.finish().await);
        assert!(!coordinator.finish().await);
        assert!(coordinator.begin().await);
    }

    #[tokio::test]
    async fn atomic_person_asset_write_skips_identical_content_and_replaces_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::MetadataExt;

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("profile.bin");
        assert!(write_atomically_if_changed(&path, b"first").await?);
        let first_inode = tokio::fs::metadata(&path).await?.ino();
        assert!(!write_atomically_if_changed(&path, b"first").await?);
        assert_eq!(tokio::fs::metadata(&path).await?.ino(), first_inode);
        assert_eq!(tokio::fs::metadata(&path).await?.mode() & 0o777, 0o600);

        assert!(write_atomically_if_changed(&path, b"second").await?);
        assert_eq!(tokio::fs::read(&path).await?, b"second");
        Ok(())
    }

    #[tokio::test]
    async fn atomic_person_asset_write_rejects_symlink_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("target.bin");
        let symlink = directory.path().join("profile.bin");
        let non_file = directory.path().join("profile-directory");
        tokio::fs::write(&target, b"target").await?;
        tokio::fs::symlink(&target, &symlink).await?;
        tokio::fs::create_dir(&non_file).await?;

        let error = write_atomically_if_changed(&symlink, b"replacement")
            .await
            .expect_err("symlink targets must remain rejected");
        assert!(matches!(error, PeopleError::Symlink(_)));
        let error = write_atomically_if_changed(&non_file, b"replacement")
            .await
            .expect_err("non-file targets must remain rejected");
        assert!(matches!(error, PeopleError::Serialization(_)));
        assert_eq!(tokio::fs::read(target).await?, b"target");
        Ok(())
    }

    #[tokio::test]
    async fn local_actor_relation_page_cache_lists_legacy_items_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let legacy_items = directory.path().join("people/items");
        tokio::fs::create_dir_all(&legacy_items).await?;
        tokio::fs::write(legacy_items.join("episode-1.json"), b"[]").await?;
        tokio::fs::write(legacy_items.join("episode-2.json"), b"[]").await?;

        let service = PeopleService::new(directory.path().to_owned());
        let cache = LocalActorRelationPageCache::new(
            ["episode-1", "episode-2", "episode-3"]
                .into_iter()
                .map(str::to_owned),
        );
        let (first, second, missing) = tokio::join!(
            service.item_actor_relation_exists_with_page_cache("episode-1", &cache),
            service.item_actor_relation_exists_with_page_cache("episode-2", &cache),
            service.item_actor_relation_exists_with_page_cache("episode-3", &cache),
        );
        let (first_exists, first_loaded) = first?;
        let (second_exists, second_loaded) = second?;
        let (missing_exists, missing_loaded) = missing?;

        assert!(first_exists);
        assert!(second_exists);
        assert!(!missing_exists);
        assert_eq!(
            [first_loaded, second_loaded, missing_loaded]
                .into_iter()
                .filter(|loaded| *loaded)
                .count(),
            1,
            "a page should enumerate its legacy relation directory only once"
        );
        Ok(())
    }

    #[tokio::test]
    async fn local_actor_relation_page_cache_falls_back_when_legacy_directory_cannot_be_read()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let legacy_people = directory.path().join("people");
        tokio::fs::create_dir_all(&legacy_people).await?;
        tokio::fs::write(legacy_people.join("items"), b"not a directory").await?;

        let service = PeopleService::new(directory.path().to_owned());
        let cache = LocalActorRelationPageCache::new(["episode-1".to_owned()]);
        let result = service
            .item_actor_relation_exists_with_page_cache("episode-1", &cache)
            .await;

        assert!(matches!(result, Err(PeopleError::Io { .. })));
        Ok(())
    }

    #[tokio::test]
    async fn local_actor_relation_page_cache_falls_back_after_legacy_entry_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let legacy_items = directory.path().join("people/items");
        tokio::fs::create_dir_all(&legacy_items).await?;
        let entries_path = legacy_items.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            for index in 0..=MAX_LOCAL_LEGACY_RELATION_DIRECTORY_ENTRIES {
                std::fs::write(entries_path.join(format!("unrelated-{index}.json")), b"[]")?;
            }
            Ok(())
        })
        .await??;

        let service = PeopleService::new(directory.path().to_owned());
        let relevant_item_ids = HashSet::from(["episode-target".to_owned()]);
        let snapshot = service
            .read_legacy_item_relation_ids(&relevant_item_ids)
            .await?;
        assert!(
            !snapshot.complete,
            "the directory scan must stop at its cap"
        );
        assert!(!snapshot.item_ids.contains("episode-target"));

        tokio::fs::write(legacy_items.join("episode-target.json"), b"[]").await?;
        let cache = LocalActorRelationPageCache::new(["episode-target".to_owned()]);
        assert!(
            cache.legacy_item_ids.set(Ok(snapshot)).is_ok(),
            "the fixture snapshot should initialize the page cache"
        );
        let (exists, _) = service
            .item_actor_relation_exists_with_page_cache("episode-target", &cache)
            .await?;

        assert!(
            exists,
            "an incomplete snapshot must fall back to the legacy path"
        );
        Ok(())
    }

    #[tokio::test]
    async fn person_asset_locks_are_sharded_by_person() {
        let service = PeopleService::new(
            tempfile::tempdir()
                .expect("temporary directory")
                .path()
                .to_owned(),
        );
        let first = {
            let mut locks = service.person_asset_locks.lock().await;
            locks
                .entry("lux-000001".to_owned())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let second = {
            let mut locks = service.person_asset_locks.lock().await;
            locks
                .entry("lux-000002".to_owned())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        assert!(!Arc::ptr_eq(&first, &second));
        let _first_guard = first.lock().await;
        assert!(second.try_lock().is_ok());
    }

    #[tokio::test]
    async fn restarting_people_recovery_skips_unchanged_manifests()
    -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        let actor = ActorCredit {
            id: "57975".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: "华晨宇".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: None,
        };
        let identities = super::actor_identities(&actor, "tmdb");
        let person_id = service
            .resolve_person_key(&actor, &identities, None)
            .await?
            .ok_or("missing canonical person")?;
        service
            .persist_person_assets(&actor, "tmdb", "57975", Some(&person_id), &identities)
            .await;

        service.rebuild_person_credit_index().await?;
        sqlx::query("UPDATE people SET updated_at = 1 WHERE id = ?")
            .bind(&person_id)
            .execute(database.pool())
            .await?;

        service.rebuild_person_credit_index().await?;
        let updated_at: i64 = sqlx::query_scalar("SELECT updated_at FROM people WHERE id = ?")
            .bind(&person_id)
            .fetch_one(database.pool())
            .await?;
        assert_eq!(updated_at, 1);

        assert!(
            !database
                .person_manifest_restore_needed(PERSON_MANIFEST_SCHEMA_VERSION as i64)
                .await?
        );
        service
            .persist_person_assets(&actor, "tmdb", "57975", Some(&person_id), &identities)
            .await;
        assert!(
            !database
                .person_manifest_restore_needed(PERSON_MANIFEST_SCHEMA_VERSION as i64)
                .await?
        );

        service
            .set_person_field_locks(&person_id, &["name".to_owned()], "{}")
            .await?;
        sqlx::query("UPDATE people SET updated_at = 1 WHERE id = ?")
            .bind(&person_id)
            .execute(database.pool())
            .await?;
        service.rebuild_person_credit_index().await?;
        let updated_at: i64 = sqlx::query_scalar("SELECT updated_at FROM people WHERE id = ?")
            .bind(&person_id)
            .fetch_one(database.pool())
            .await?;
        assert!(updated_at > 1);
        Ok(())
    }

    #[tokio::test]
    async fn restoring_unchanged_person_manifests_batches_checksum_queries()
    -> Result<(), Box<dyn std::error::Error>> {
        const MANIFEST_COUNT: usize = 205;

        let config_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());

        for index in 0..MANIFEST_COUNT {
            let person_id = format!("lux-{index:06}");
            let display_name = format!("Person {index}");
            let person_dir = lux_person_directory(&config.config_dir, &display_name, &person_id)?;
            tokio::fs::create_dir_all(&person_dir).await?;
            let mut manifest = PersonManifest {
                schema_version: PERSON_MANIFEST_SCHEMA_VERSION,
                lux_person_id: person_id.clone(),
                display_name,
                ..PersonManifest::default()
            };
            let unsigned_bytes = serde_json::to_vec(&manifest)?;
            manifest.checksum = Sha256::digest(unsigned_bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            tokio::fs::write(
                person_dir.join(PERSON_MANIFEST),
                serde_json::to_vec(&manifest)?,
            )
            .await?;
            sqlx::query(
                "INSERT INTO people (
                    id, display_name, directory_name, normalized_name,
                    status, created_at, updated_at
                 ) VALUES (?, ?, ?, ?, 'ACTIVE', 1, 1)",
            )
            .bind(&person_id)
            .bind(&manifest.display_name)
            .bind(&manifest.display_name)
            .bind(manifest.display_name.to_lowercase())
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO person_manifest_index_state (
                    person_id, manifest_checksum, manifest_schema_version, updated_at
                 ) VALUES (?, ?, ?, 1)",
            )
            .bind(person_id)
            .bind(&manifest.checksum)
            .bind(i64::from(PERSON_MANIFEST_SCHEMA_VERSION))
            .execute(database.pool())
            .await?;
        }

        database.reset_query_count();
        let report = service.restore_person_manifests(&database).await?;

        assert_eq!(report.restored, 0);
        assert!(!report.failed);
        assert_eq!(database.query_count(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn nfo_actor_without_identity_keeps_the_existing_profile_after_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind("item-1")
        .bind(library.id.to_string())
        .bind("测试电影")
        .bind("测试电影")
        .execute(database.pool())
        .await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        let identified_actor = ActorCredit {
            id: "57975".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: "华晨宇".to_owned(),
            character: Some("角色甲".to_owned()),
            order: Some(0),
            profile_url: None,
            person: None,
        };
        let identities = super::actor_identities(&identified_actor, "tmdb");
        let person_id = service
            .resolve_person_key(&identified_actor, &identities, None)
            .await?
            .ok_or("missing canonical person")?;
        let person_dir = lux_person_directory(&config.config_dir, "华晨宇", &person_id)?;
        tokio::fs::create_dir_all(&person_dir).await?;
        tokio::fs::write(person_dir.join("folder.png"), PNG_1X1).await?;

        service
            .persist_item_actors("item-1", "tmdb", &[identified_actor])
            .await?;
        assert_eq!(
            service.list_item_actors("item-1").await?[0]
                .image_url
                .as_deref(),
            Some("/api/v1/people/57975/image")
        );

        let nfo_actor_without_identity = ActorCredit {
            id: String::new(),
            provider: None,
            identities: Vec::new(),
            name: "华晨宇".to_owned(),
            character: Some("角色甲".to_owned()),
            order: Some(0),
            profile_url: None,
            person: None,
        };
        service
            .persist_nfo_item_actors("item-1", "tmdb", &[nfo_actor_without_identity], &[1, 2, 3])
            .await?;

        let restarted = PeopleService::new(config.config_dir.clone()).with_database(database);
        assert_eq!(
            restarted.list_item_actors("item-1").await?[0]
                .image_url
                .as_deref(),
            Some("/api/v1/people/57975/image")
        );
        Ok(())
    }

    #[tokio::test]
    async fn legacy_person_migration_stays_completed_when_manifest_restore_requeues()
    -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let legacy_dir = people_directory(&config.config_dir, "旧人物", "tmdb", "57975")?;
        tokio::fs::create_dir_all(&legacy_dir).await?;
        tokio::fs::write(
            legacy_dir.join(PERSON_NFO),
            r#"<?xml version="1.0"?><person><name>旧人物</name><uniqueid type="tmdb">57975</uniqueid></person>"#
                .as_bytes(),
        )
        .await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());

        service.rebuild_person_credit_index().await?;
        let migration_status: Option<String> =
            sqlx::query_scalar("SELECT status FROM legacy_person_migration_state WHERE id = 1")
                .fetch_optional(database.pool())
                .await?;
        assert_eq!(migration_status.as_deref(), Some("COMPLETED"));

        let later_legacy_dir = people_directory(&config.config_dir, "后来人物", "tmdb", "57976")?;
        tokio::fs::create_dir_all(&later_legacy_dir).await?;
        tokio::fs::write(
            later_legacy_dir.join(PERSON_NFO),
            r#"<?xml version="1.0"?><person><name>后来人物</name><uniqueid type="tmdb">57976</uniqueid></person>"#
                .as_bytes(),
        )
        .await?;
        database
            .mark_person_manifest_restore_pending(PERSON_MANIFEST_SCHEMA_VERSION as i64)
            .await?;
        service.rebuild_person_credit_index().await?;

        let migration_status: Option<String> =
            sqlx::query_scalar("SELECT status FROM legacy_person_migration_state WHERE id = 1")
                .fetch_optional(database.pool())
                .await?;
        assert_eq!(migration_status.as_deref(), Some("COMPLETED"));
        assert!(
            database
                .find_canonical_person_by_identity("tmdb", "57976")
                .await?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn birthday_matching_accepts_format_variants_but_rejects_full_date_conflicts() {
        assert!(super::birthdays_compatible(
            Some("1990-01-02"),
            Some("1990年1月2日")
        ));
        assert!(!super::birthdays_compatible(
            Some("1990-01-02"),
            Some("1991-01-02")
        ));
        assert!(super::birthdays_compatible(
            Some("1990-01"),
            Some("1990-01-02")
        ));
    }

    #[test]
    fn relation_matching_rejects_a_replaced_file_at_the_same_path() {
        let relation = super::StoredPeopleRelation {
            schema_version: 4,
            generation: 1,
            source_fingerprint: None,
            item_id: Some("old-item".to_owned()),
            source_key: Some("old-source".to_owned()),
            source_root: Some("/library".to_owned()),
            source_relative_path: Some("movie.mkv".to_owned()),
            media_fingerprint: Some(super::encode_fingerprint(b"old-fingerprint")),
            media_size: Some(100),
            media_modified_at: Some(10),
            media_title: Some("Old Movie".to_owned()),
            media_production_year: Some(2020),
            actors: Vec::new(),
        };
        let current = crate::storage::StoredItemSourceLocator {
            root_path: "/library".to_owned(),
            relative_path: "movie.mkv".to_owned(),
            fingerprint: Some(b"new-fingerprint".to_vec()),
            size: 100,
            modified_at: 10,
            title: "Old Movie".to_owned(),
            production_year: Some(2020),
        };

        assert!(!super::relation_source_snapshot_matches(
            &relation, &current
        ));
        assert!(!super::relation_media_snapshot_matches(&relation, &current));
        let moved = crate::storage::StoredItemSourceLocator {
            root_path: "/new-library".to_owned(),
            relative_path: "renamed.mkv".to_owned(),
            fingerprint: Some(b"old-fingerprint".to_vec()),
            ..current
        };
        assert!(!super::relation_source_snapshot_matches(&relation, &moved));
        assert!(super::relation_media_snapshot_matches(&relation, &moved));
    }

    #[tokio::test]
    async fn database_backed_people_reuse_lux_id_when_provider_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        let first = ActorCredit {
            id: "57975".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: "华晨宇".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: None,
        };
        let first_identities = super::actor_identities(&first, "tmdb");
        let first_person = service
            .resolve_person_key(&first, &first_identities, None)
            .await?
            .ok_or("first person was not created")?;
        assert_eq!(first_person, "lux-000001");
        let _first_assets = service
            .persist_person_assets(
                &first,
                "tmdb",
                "57975",
                Some(&first_person),
                &first_identities,
            )
            .await;
        let person_dir = lux_person_directory(&config.config_dir, "华晨宇", &first_person)?;
        assert!(person_dir.join("person.json").exists());
        let first_manifest: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(person_dir.join("person.json")).await?)?;
        assert_eq!(first_manifest["generation"], 1);

        let previous_relation = super::StoredPeopleRelation {
            schema_version: 2,
            generation: 1,
            source_fingerprint: None,
            item_id: None,
            source_key: None,
            source_root: None,
            source_relative_path: None,
            media_fingerprint: None,
            media_size: None,
            media_modified_at: None,
            media_title: None,
            media_production_year: None,
            actors: vec![super::StoredActor {
                id: Some("57975".to_owned()),
                name: "华晨宇".to_owned(),
                provider: "tmdb".to_owned(),
                person_key: Some(first_person.clone()),
                lux_person_id: Some(first_person.clone()),
                identities: first_identities.clone(),
                character: None,
                order: Some(0),
                image_file: None,
                pending_assets: Vec::new(),
                person: None,
            }],
        };
        let bridge_actor = ActorCredit {
            id: "1313123".to_owned(),
            provider: Some("douban".to_owned()),
            identities: Vec::new(),
            name: "华晨宇".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: None,
        };
        let bridge_identities = super::actor_identities(&bridge_actor, "douban");
        let bridge_candidates =
            super::same_media_bridge_candidates(Some(&previous_relation), &bridge_actor);
        let bridge_key = bridge_candidates
            .first()
            .and_then(|candidate| candidate.person_key.as_deref())
            .ok_or("same-media bridge was not selected")?;
        let bridged_person = service
            .resolve_person_key(&bridge_actor, &bridge_identities, Some(bridge_key))
            .await?
            .ok_or("same-media bridge did not resolve")?;
        assert_eq!(bridged_person, first_person);

        let second = ActorCredit {
            id: "1313123".to_owned(),
            provider: Some("douban".to_owned()),
            identities: vec![PersonIdentity {
                provider: "tmdb".to_owned(),
                id: "57975".to_owned(),
            }],
            name: "华晨宇".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: None,
        };
        let second_identities = super::actor_identities(&second, "douban");
        let second_person = service
            .resolve_person_key(&second, &second_identities, None)
            .await?
            .ok_or("second person was not resolved")?;
        assert_eq!(second_person, first_person);
        service
            .persist_person_assets(
                &second,
                "douban",
                "1313123",
                Some(&second_person),
                &second_identities,
            )
            .await;
        let manifest: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(person_dir.join("person.json")).await?)?;
        assert_eq!(manifest["luxPersonId"], "lux-000001");
        assert_eq!(manifest["identities"].as_array().map(Vec::len), Some(2));
        assert_eq!(manifest["generation"], 2);
        let nfo = tokio::fs::read_to_string(person_dir.join("person.nfo")).await?;
        assert!(nfo.contains("type=\"tmdb\">57975"));
        assert!(nfo.contains("type=\"douban\">1313123"));

        sqlx::query("DELETE FROM person_identities")
            .execute(database.pool())
            .await?;
        sqlx::query("DELETE FROM people")
            .execute(database.pool())
            .await?;
        assert_eq!(
            service.restore_person_manifests(&database).await?.restored,
            1,
            "manifest should restore one canonical person"
        );
        let restored = database
            .find_canonical_person_by_identity("douban", "1313123")
            .await?
            .ok_or("restored provider identity was not indexed")?;
        assert_eq!(restored.id, "lux-000001");
        Ok(())
    }

    #[tokio::test]
    async fn lux_person_first_write_migrates_existing_legacy_assets()
    -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database);
        let actor = ActorCredit {
            id: "57975".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: "华晨宇".to_owned(),
            character: None,
            order: None,
            profile_url: None,
            person: None,
        };
        let identities = super::actor_identities(&actor, "tmdb");
        let lux_person_id = service
            .resolve_person_key(&actor, &identities, None)
            .await?
            .ok_or("missing Lux person")?;
        let legacy_dir = people_directory(&config.config_dir, "华晨宇", "tmdb", "57975")?;
        tokio::fs::create_dir_all(&legacy_dir).await?;
        tokio::fs::write(
            legacy_dir.join("person.nfo"),
            r#"<?xml version="1.0"?><person><name>旧姓名</name><biography>旧简介</biography></person>"#
                .as_bytes(),
        )
        .await?;
        tokio::fs::write(legacy_dir.join("folder.png"), PNG_1X1).await?;

        service
            .persist_person_assets(&actor, "tmdb", "57975", Some(&lux_person_id), &identities)
            .await;

        let target_dir = lux_person_directory(&config.config_dir, "华晨宇", &lux_person_id)?;
        let nfo = tokio::fs::read_to_string(target_dir.join("person.nfo")).await?;
        assert!(nfo.contains("旧简介"));
        assert_eq!(
            tokio::fs::read(target_dir.join("folder.png")).await?,
            PNG_1X1
        );
        assert!(legacy_dir.join("person.nfo").exists());
        assert!(legacy_dir.join("folder.png").exists());
        Ok(())
    }

    #[tokio::test]
    async fn rebuilding_people_migrates_legacy_nfo_without_a_manifest()
    -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let legacy_dir = people_directory(&config.config_dir, "旧人物", "tmdb", "57975")?;
        tokio::fs::create_dir_all(&legacy_dir).await?;
        tokio::fs::write(
            legacy_dir.join("person.nfo"),
            r#"<?xml version="1.0"?><person><name>旧人物</name><uniqueid type="tmdb">57975</uniqueid></person>"#
                .as_bytes(),
        )
        .await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        assert_eq!(
            service.restore_legacy_person_directories(&database).await?,
            1
        );
        assert_eq!(
            service.restore_legacy_person_directories(&database).await?,
            0
        );
        let person = database
            .find_canonical_person_by_identity("tmdb", "57975")
            .await?
            .ok_or("legacy provider identity was not restored")?;
        assert_eq!(person.id, "lux-000001");
        let next = database
            .resolve_or_create_canonical_person(
                "新人物",
                "tmdb",
                "57976",
                "PROVIDER_ID",
                Some(1.0),
                r#"{"method":"test"}"#,
            )
            .await?;
        assert_eq!(next.id, "lux-000002");
        let target = lux_person_directory(&config.config_dir, "旧人物", &person.id)?;
        assert!(target.join(super::PERSON_MANIFEST).exists());
        assert!(target.join(super::PERSON_NFO).exists());
        Ok(())
    }

    #[tokio::test]
    async fn unique_name_and_compatible_birthday_bridge_without_media_role_data()
    -> Result<(), Box<dyn std::error::Error>> {
        let config_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        for item_id in ["item-global-first", "item-global-second"] {
            sqlx::query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
        }
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        service
            .persist_item_actors(
                "item-global-first",
                "tmdb",
                &[ActorCredit {
                    id: "57975".to_owned(),
                    provider: Some("tmdb".to_owned()),
                    identities: Vec::new(),
                    name: "同名演员".to_owned(),
                    character: None,
                    order: None,
                    profile_url: None,
                    person: Some(super::PersonMetadata {
                        birthday: Some("1970-01-02".to_owned()),
                        ..Default::default()
                    }),
                }],
            )
            .await?;
        service
            .persist_item_actors(
                "item-global-second",
                "douban",
                &[ActorCredit {
                    id: "1313123".to_owned(),
                    provider: Some("douban".to_owned()),
                    identities: Vec::new(),
                    name: "同名演员".to_owned(),
                    character: None,
                    order: None,
                    profile_url: None,
                    person: Some(super::PersonMetadata {
                        birthday: Some("1970年1月2日".to_owned()),
                        ..Default::default()
                    }),
                }],
            )
            .await?;

        let first = database
            .find_canonical_person_by_identity("tmdb", "57975")
            .await?
            .ok_or("missing first provider identity")?;
        let person = database
            .find_canonical_person_by_identity("douban", "1313123")
            .await?
            .ok_or("missing bridged provider identity")?;
        assert_eq!(person.id, first.id);
        Ok(())
    }

    #[tokio::test]
    async fn identity_move_updates_person_manifest_and_keeps_event_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let actor = ActorCredit {
            id: "57975".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: "目标人物".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: None,
        };
        let identities = super::actor_identities(&actor, "tmdb");
        let person_dir = lux_person_directory(config.path(), "目标人物", "lux-000001")?;
        service
            .persist_person_assets(&actor, "tmdb", "57975", Some("lux-000001"), &identities)
            .await;
        let event = super::PersonManifestIdentityEvent {
            event_id: "event-1".to_owned(),
            event_type: "MANUAL_SPLIT".to_owned(),
            provider: "tmdb".to_owned(),
            provider_id: "57975".to_owned(),
            from_person_id: Some("lux-000001".to_owned()),
            to_person_id: Some("lux-000002".to_owned()),
            evidence_json: r#"{"reason":"test"}"#.to_owned(),
            created_at: 1,
        };
        service
            .update_person_manifest_identity(
                "lux-000001",
                Some("目标人物"),
                None,
                Some(("tmdb", "57975")),
                &event,
            )
            .await?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(person_dir.join("person.json")).await?)?;
        assert_eq!(manifest["identities"].as_array().map(Vec::len), Some(0));
        assert!(manifest["identityEvents"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|event| event["eventType"] == "MANUAL_SPLIT")
        }));
        assert_eq!(manifest["generation"], 2);
        Ok(())
    }

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
        assert_eq!(relation["schemaVersion"], 4);
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
            lux_person_id: None,
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
        use std::os::unix::fs::MetadataExt;

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

        let image_inode = tokio::fs::metadata(&person_image).await?.ino();
        let index_path = people_index_path_for_provider(config.path(), "tmdb", "9")?;
        let index_inode = tokio::fs::metadata(&index_path).await?.ino();
        let legacy_index_path =
            crate::application::metadata_paths::people_index_path(config.path(), "9")?;
        let legacy_index_inode = tokio::fs::metadata(&legacy_index_path).await?.ino();

        service
            .update_person_image("9", "演员甲", Some("tmdb"), Some("image/png"), PNG_1X1)
            .await?;

        assert_eq!(tokio::fs::metadata(&person_image).await?.ino(), image_inode);
        assert_eq!(tokio::fs::metadata(&index_path).await?.ino(), index_inode);
        assert_eq!(
            tokio::fs::metadata(&legacy_index_path).await?.ino(),
            legacy_index_inode
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
        assert_eq!(relation["generation"], 1);
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
        service
            .persist_nfo_item_actors("item-1", "tmdb", &actors, &second)
            .await?;
        let relation: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(library_item_directory(config.path(), "item-1")?.join("people.json"))
                .await?,
        )?;
        assert_eq!(relation["generation"], 2);
        Ok(())
    }

    #[tokio::test]
    async fn nfo_relation_current_requires_matching_persisted_credit_revision()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{config::Config, storage::Database};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind("item-1")
        .bind(library.id.to_string())
        .bind("测试电影")
        .bind("测试电影")
        .execute(database.pool())
        .await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        let source_fingerprint = [1_u8; 32];
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
            .persist_nfo_item_actors("item-1", "tmdb", &actors, &source_fingerprint)
            .await?;
        assert_relation_checksum_matches_file(&database, &config.config_dir, "item-1").await?;

        let enriched_actor = ActorCredit {
            id: "9".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: "演员甲".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: Some(PersonMetadata {
                biography: Some("在线补全的人物简介".to_owned()),
                ..PersonMetadata::default()
            }),
        };
        service
            .update_item_actor_metadata("item-1", "tmdb", &[enriched_actor])
            .await?;
        assert_relation_checksum_matches_file(&database, &config.config_dir, "item-1").await?;

        service
            .update_person_metadata(
                &[library.id.to_string()],
                "9",
                PersonMetadataUpdate {
                    name: "演员甲".to_owned(),
                    biography: Some("人物资料更新后的简介".to_owned()),
                    ..PersonMetadataUpdate::default()
                },
            )
            .await?;
        assert_relation_checksum_matches_file(&database, &config.config_dir, "item-1").await?;

        assert!(
            service
                .item_actor_relation_is_current("item-1", &source_fingerprint)
                .await?
        );
        assert!(service.nfo_relation_snapshot_is_current("item-1").await?);

        sqlx::query(
            "UPDATE person_index_item_state SET relation_checksum = NULL WHERE item_id = ?",
        )
        .bind("item-1")
        .execute(database.pool())
        .await?;
        assert!(
            !service
                .item_actor_relation_is_current("item-1", &source_fingerprint)
                .await?,
            "legacy checksum-free index state must be rebuilt"
        );
        assert!(!service.nfo_relation_snapshot_is_current("item-1").await?);
        service
            .persist_nfo_item_actors("item-1", "tmdb", &actors, &source_fingerprint)
            .await?;
        assert!(service.nfo_relation_snapshot_is_current("item-1").await?);

        let relation_path =
            library_item_directory(&config.config_dir, "item-1")?.join("people.json");
        let relation_bytes = tokio::fs::read(&relation_path).await?;
        let changed_bytes = [relation_bytes.as_slice(), b"\n"].concat();
        tokio::fs::write(&relation_path, changed_bytes).await?;
        assert!(
            !service
                .item_actor_relation_is_current("item-1", &source_fingerprint)
                .await?,
            "changed relation bytes must be stale even when the source fingerprint is unchanged"
        );
        assert!(
            !service.nfo_relation_snapshot_is_current("item-1").await?,
            "NFO relation reuse must compare the persisted relation checksum"
        );
        tokio::fs::write(&relation_path, &relation_bytes).await?;
        assert!(service.nfo_relation_snapshot_is_current("item-1").await?);

        database.clear_person_credits("item-1").await?;

        assert!(
            !service
                .item_actor_relation_is_current("item-1", &source_fingerprint)
                .await?,
            "relation file alone must not hide a missing credit-index revision"
        );

        service
            .persist_nfo_item_actors("item-1", "tmdb", &actors, &source_fingerprint)
            .await?;
        assert!(
            service
                .item_actor_relation_is_current("item-1", &source_fingerprint)
                .await?
        );
        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn relation_rebuild_does_not_skip_changed_bytes_with_same_source_fingerprint()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{config::Config, storage::Database};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind("item-rebuild")
        .bind(library.id.to_string())
        .bind("测试电影")
        .bind("测试电影")
        .execute(database.pool())
        .await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        let source_fingerprint = [1_u8, 2, 3];
        let actors = [ActorCredit {
            id: "9".to_owned(),
            provider: None,
            identities: Vec::new(),
            name: "演员甲".to_owned(),
            character: Some("角色甲".to_owned()),
            order: Some(0),
            profile_url: None,
            person: None,
        }];
        service
            .persist_nfo_item_actors("item-rebuild", "tmdb", &actors, &source_fingerprint)
            .await?;
        service.rebuild_person_credit_index().await?;

        let relation_path =
            library_item_directory(&config.config_dir, "item-rebuild")?.join("people.json");
        let relation_bytes = tokio::fs::read(&relation_path).await?;
        let mut relation: serde_json::Value = serde_json::from_slice(&relation_bytes)?;
        relation["actors"][0]["name"] = serde_json::Value::String("演员乙".to_owned());
        tokio::fs::write(&relation_path, serde_json::to_vec_pretty(&relation)?).await?;

        sqlx::query("UPDATE person_index_item_state SET updated_at = 1 WHERE item_id = ?")
            .bind("item-rebuild")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "UPDATE person_index_rebuild_jobs
             SET status = 'QUEUED', cursor_id = NULL, processed_count = 1,
                 total_count = 1, cancel_requested = 0, run_token = NULL
             WHERE library_id = ?",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

        assert_eq!(service.rebuild_person_credit_index().await?, 1);
        let updated_at: i64 =
            sqlx::query_scalar("SELECT updated_at FROM person_index_item_state WHERE item_id = ?")
                .bind("item-rebuild")
                .fetch_one(database.pool())
                .await?;
        assert!(
            updated_at > 1,
            "changed snapshot bytes must trigger rebuilding"
        );
        assert_relation_checksum_matches_file(&database, &config.config_dir, "item-rebuild")
            .await?;
        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn local_nfo_page_coalesces_duplicate_person_asset_persistence()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::MetadataExt;

        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let deferred = DeferredNfoActorCredits::default();
        let actor = ActorCredit {
            id: "9".to_owned(),
            provider: None,
            identities: Vec::new(),
            name: "演员甲".to_owned(),
            character: Some("角色甲".to_owned()),
            order: Some(0),
            profile_url: None,
            person: None,
        };

        assert_eq!(
            service
                .persist_nfo_item_actors_deferred(
                    "item-a",
                    "tmdb",
                    std::slice::from_ref(&actor),
                    &[1, 2, 3],
                    &deferred,
                )
                .await?
                .stored_count,
            1
        );

        let first_relation: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(library_item_directory(config.path(), "item-a")?.join("people.json"))
                .await?,
        )?;
        let person_key = first_relation["actors"][0]["personKey"]
            .as_str()
            .ok_or("missing resolved person key")?;
        let person_nfo = canonical_person_directory(config.path(), person_key)?.join("person.nfo");
        let first_inode = tokio::fs::metadata(&person_nfo).await?.ino();

        assert_eq!(
            service
                .persist_nfo_item_actors_deferred(
                    "item-b",
                    "tmdb",
                    std::slice::from_ref(&actor),
                    &[4, 5, 6],
                    &deferred,
                )
                .await?
                .stored_count,
            1
        );
        assert_eq!(
            tokio::fs::metadata(&person_nfo).await?.ino(),
            first_inode,
            "the same page should persist duplicate person assets only once"
        );

        for item_id in ["item-a", "item-b"] {
            let relation_path = library_item_directory(config.path(), item_id)?.join("people.json");
            let relation: serde_json::Value =
                serde_json::from_slice(&tokio::fs::read(relation_path).await?)?;
            assert_eq!(relation["actors"].as_array().map(Vec::len), Some(1));
        }

        let changed_actor = ActorCredit {
            person: Some(PersonMetadata {
                biography: Some("新增人物简介".to_owned()),
                ..PersonMetadata::default()
            }),
            ..actor
        };
        service
            .persist_nfo_item_actors_deferred(
                "item-c",
                "tmdb",
                &[changed_actor],
                &[7, 8, 9],
                &deferred,
            )
            .await?;
        let updated_metadata = tokio::fs::metadata(&person_nfo).await?;
        assert_ne!(updated_metadata.ino(), first_inode);
        assert!(
            tokio::fs::read_to_string(person_nfo)
                .await?
                .contains("<biography>新增人物简介</biography>")
        );
        Ok(())
    }

    #[tokio::test]
    async fn local_nfo_person_asset_batch_isolates_failures_between_people()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let deferred = DeferredNfoActorCredits::default();
        let profiles = config.path().join("people/profiles");
        tokio::fs::create_dir_all(&profiles).await?;
        tokio::fs::write(profiles.join("9.png"), PNG_1X1).await?;
        tokio::fs::write(profiles.join("11.png"), PNG_1X1).await?;

        let indexed_profile = Path::new("people/assets/provider-seed.png");
        let indexed_profile_path = metadata_root(config.path()).join(indexed_profile);
        tokio::fs::create_dir_all(
            indexed_profile_path
                .parent()
                .ok_or("indexed profile path has no parent")?,
        )
        .await?;
        tokio::fs::write(&indexed_profile_path, PNG_1X1).await?;
        let readable_provider_index = people_index_path_for_provider(config.path(), "aaa", "10")?;
        tokio::fs::create_dir_all(
            readable_provider_index
                .parent()
                .ok_or("readable provider index path has no parent")?,
        )
        .await?;
        tokio::fs::write(
            &readable_provider_index,
            serde_json::to_vec(&serde_json::json!({
                "imagePath": indexed_profile.to_string_lossy(),
            }))?,
        )
        .await?;
        let broken_index = people_index_path_for_provider(config.path(), "tmdb", "10")?;
        tokio::fs::create_dir_all(
            broken_index
                .parent()
                .ok_or("provider index path has no parent")?,
        )
        .await?;
        let index_target = config.path().join("index-target.json");
        tokio::fs::write(&index_target, b"{}").await?;
        tokio::fs::symlink(index_target, &broken_index).await?;

        let actors =
            [("9", "演员甲"), ("10", "演员乙"), ("11", "演员丙")].map(|(id, name)| ActorCredit {
                id: id.to_owned(),
                provider: None,
                identities: if id == "10" {
                    vec![
                        PersonIdentity {
                            provider: "aaa".to_owned(),
                            id: id.to_owned(),
                        },
                        PersonIdentity {
                            provider: "tmdb".to_owned(),
                            id: id.to_owned(),
                        },
                    ]
                } else {
                    Vec::new()
                },
                name: name.to_owned(),
                character: None,
                order: None,
                profile_url: None,
                person: None,
            });

        let report = service
            .persist_nfo_item_actors_deferred("item-assets", "tmdb", &actors, &[1, 2, 3], &deferred)
            .await?;
        assert_eq!(report.stored_count, 3);
        assert!(report.pending_assets.iter().any(|item_id| item_id == "10"));

        let relation_path =
            library_item_directory(config.path(), "item-assets")?.join("people.json");
        let relation: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(relation_path).await?)?;
        let stored_actors = relation["actors"].as_array().ok_or("missing actors")?;
        assert_eq!(stored_actors.len(), 3);
        for name in ["演员甲", "演员丙"] {
            let actor = stored_actors
                .iter()
                .find(|actor| actor["name"] == name)
                .ok_or("missing successfully persisted actor")?;
            let person_key = actor["personKey"].as_str().ok_or("missing person key")?;
            assert!(
                canonical_person_directory(config.path(), person_key)?
                    .join("person.nfo")
                    .exists()
            );
        }
        let failed_actor = stored_actors
            .iter()
            .find(|actor| actor["name"] == "演员乙")
            .ok_or("missing isolated failed actor")?;
        assert!(
            failed_actor["pendingAssets"]
                .as_array()
                .is_some_and(|pending| pending.iter().any(|asset| asset == PENDING_PERSON_INDEX))
        );
        Ok(())
    }

    #[tokio::test]
    async fn local_nfo_person_assets_keep_page_concurrency_at_four()
    -> Result<(), Box<dyn std::error::Error>> {
        const PERSON_COUNT: usize = 8;
        const EXPECTED_CONCURRENCY: usize = LOCAL_NFO_PERSON_ASSET_BATCH_CONCURRENCY;

        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let mut deferred = DeferredNfoActorCredits::default();
        let (started, mut started_receiver) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        deferred.person_asset_test_probe = Some(LocalNfoPersonAssetConcurrencyProbe {
            started,
            release: Arc::clone(&release),
            active: Arc::clone(&active),
            max_active: Arc::clone(&max_active),
        });

        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..PERSON_COUNT {
            let service = service.clone();
            let deferred = deferred.clone();
            let item_id = format!("item-concurrency-{index}");
            let actor = ActorCredit {
                id: (index + 1).to_string(),
                provider: None,
                identities: Vec::new(),
                name: format!("演员{index}"),
                character: None,
                order: None,
                profile_url: None,
                person: None,
            };
            tasks.spawn(async move {
                service
                    .persist_nfo_item_actors_deferred(
                        &item_id,
                        "tmdb",
                        &[actor],
                        &[index as u8],
                        &deferred,
                    )
                    .await
            });
        }

        for _ in 0..EXPECTED_CONCURRENCY {
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(5), started_receiver.recv(),)
                    .await?,
                Some(()),
                "expected each available page permit to admit one person asset task"
            );
        }
        assert_eq!(active.load(Ordering::SeqCst), EXPECTED_CONCURRENCY);
        assert!(
            deferred.person_asset_permits.try_acquire().is_err(),
            "all page permits should remain held while the admitted tasks are blocked"
        );

        release.add_permits(PERSON_COUNT);
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result??.stored_count, 1);
        }
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(max_active.load(Ordering::SeqCst), EXPECTED_CONCURRENCY);
        Ok(())
    }

    #[tokio::test]
    async fn local_nfo_person_nfo_and_provider_index_skip_unchanged_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::MetadataExt;

        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let actor = ActorCredit {
            id: "9".to_owned(),
            provider: None,
            identities: Vec::new(),
            name: "演员甲".to_owned(),
            character: None,
            order: None,
            profile_url: None,
            person: None,
        };

        service
            .persist_nfo_item_actors("item-first", "tmdb", std::slice::from_ref(&actor), &[1])
            .await?;
        let first_relation: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(
                library_item_directory(config.path(), "item-first")?.join("people.json"),
            )
            .await?,
        )?;
        let person_key = first_relation["actors"][0]["personKey"]
            .as_str()
            .ok_or("missing resolved person key")?;
        let person_dir = canonical_person_directory(config.path(), person_key)?;
        let person_nfo = person_dir.join("person.nfo");
        let first_nfo_inode = tokio::fs::metadata(&person_nfo).await?.ino();
        tokio::fs::write(person_dir.join("folder.png"), PNG_1X1).await?;

        service
            .persist_nfo_item_actors("item-second", "tmdb", std::slice::from_ref(&actor), &[2])
            .await?;
        assert_eq!(
            tokio::fs::metadata(&person_nfo).await?.ino(),
            first_nfo_inode,
            "an unchanged person NFO must not be atomically replaced"
        );

        let provider_index = people_index_path_for_provider(config.path(), "tmdb", "9")?;
        let first_index_inode = tokio::fs::metadata(&provider_index).await?.ino();
        service
            .persist_nfo_item_actors("item-third", "tmdb", &[actor], &[3])
            .await?;
        assert_eq!(
            tokio::fs::metadata(provider_index).await?.ino(),
            first_index_inode,
            "an unchanged provider index must not be atomically replaced"
        );
        Ok(())
    }

    #[tokio::test]
    async fn local_nfo_lux_person_canonical_index_skips_unchanged_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::MetadataExt;

        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let person_key = "lux-000001";
        let person_dir = lux_person_directory(config.path(), "演员甲", person_key)?;
        tokio::fs::create_dir_all(&person_dir).await?;
        tokio::fs::write(person_dir.join("folder.png"), PNG_1X1).await?;

        let actor = ActorCredit {
            id: "9".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: Vec::new(),
            name: "演员甲".to_owned(),
            character: None,
            order: None,
            profile_url: None,
            person: None,
        };
        let identities = [PersonIdentity {
            provider: "tmdb".to_owned(),
            id: "9".to_owned(),
        }];
        let index_path = people_index_path(config.path(), person_key)?;

        service
            .persist_person_assets(&actor, "tmdb", "9", Some(person_key), &identities)
            .await;
        let first_index_inode = tokio::fs::metadata(&index_path).await?.ino();
        service
            .persist_person_assets(&actor, "tmdb", "9", Some(person_key), &identities)
            .await;
        assert_eq!(
            tokio::fs::metadata(&index_path).await?.ino(),
            first_index_inode,
            "an unchanged canonical person index must not be atomically replaced"
        );

        let nfo_path = person_dir.join(PERSON_NFO);
        let first_nfo_inode = tokio::fs::metadata(&nfo_path).await?.ino();
        let updated_actor = ActorCredit {
            person: Some(PersonMetadata {
                biography: Some("新增人物简介".to_owned()),
                ..PersonMetadata::default()
            }),
            ..actor
        };
        service
            .persist_person_assets(&updated_actor, "tmdb", "9", Some(person_key), &identities)
            .await;
        assert_ne!(tokio::fs::metadata(&nfo_path).await?.ino(), first_nfo_inode);
        assert!(
            tokio::fs::read_to_string(nfo_path)
                .await?
                .contains("<biography>新增人物简介</biography>")
        );
        assert_eq!(
            tokio::fs::metadata(index_path).await?.ino(),
            first_index_inode,
            "metadata-only changes must leave unchanged canonical index bytes in place"
        );
        Ok(())
    }

    #[tokio::test]
    async fn deferred_nfo_actor_credits_become_current_after_batch_flush()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{config::Config, storage::Database};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        for item_id in ["item-a", "item-b"] {
            sqlx::query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
        }
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
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
        let fingerprint_a = [1_u8, 2, 3];
        let fingerprint_b = [4_u8, 5, 6];
        let deferred_credits = DeferredNfoActorCredits::default();
        service
            .persist_nfo_item_actors_deferred(
                "item-a",
                "tmdb",
                &actors,
                &fingerprint_a,
                &deferred_credits,
            )
            .await?;
        service
            .persist_nfo_item_actors_deferred(
                "item-b",
                "tmdb",
                &actors,
                &fingerprint_b,
                &deferred_credits,
            )
            .await?;

        assert!(
            !service
                .item_actor_relation_is_current("item-a", &fingerprint_a)
                .await?
        );
        assert!(
            library_item_directory(config.config_dir.as_path(), "item-a")?
                .join("people.json")
                .exists()
        );

        assert!(
            service
                .flush_deferred_nfo_actor_credits(&deferred_credits)
                .await
                .is_empty()
        );

        assert!(
            service
                .item_actor_relation_is_current("item-a", &fingerprint_a)
                .await?
        );
        assert!(
            service
                .item_actor_relation_is_current("item-b", &fingerprint_b)
                .await?
        );
        for item_id in ["item-a", "item-b"] {
            assert_relation_checksum_matches_file(&database, &config.config_dir, item_id).await?;
        }
        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn deferred_relation_credit_failure_preserves_snapshot_for_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{config::Config, storage::Database};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-retry', ?, 'MOVIE', 'Retry', 'Retry', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        let actors = [ActorCredit {
            id: "actor-9".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: vec![PersonIdentity {
                provider: "tmdb".to_owned(),
                id: "actor-9".to_owned(),
            }],
            name: "演员甲".to_owned(),
            character: Some("角色甲".to_owned()),
            order: Some(0),
            profile_url: None,
            person: None,
        }];
        let source_fingerprint = [7_u8, 8, 9];
        let deferred_credits = DeferredNfoActorCredits::default();
        service
            .persist_nfo_item_actors_deferred(
                "item-retry",
                "tmdb",
                &actors,
                &source_fingerprint,
                &deferred_credits,
            )
            .await?;

        sqlx::query(
            "CREATE TRIGGER reject_relation_credit_state
             BEFORE INSERT ON person_index_item_state
             WHEN NEW.item_id = 'item-retry'
             BEGIN SELECT RAISE(ABORT, 'injected relation state failure'); END",
        )
        .execute(database.pool())
        .await?;
        let failures = service
            .flush_deferred_nfo_actor_credits(&deferred_credits)
            .await;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].item_ids, ["item-retry"]);
        let failed_credit_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-retry'")
                .fetch_one(database.pool())
                .await?;
        let failed_state_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM person_index_item_state WHERE item_id = 'item-retry'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(
            failed_credit_count, 0,
            "the failed DB chunk must roll back credits"
        );
        assert_eq!(
            failed_state_count, 0,
            "the failed DB chunk must roll back relation revision state"
        );

        let relation_path =
            library_item_directory(&config.config_dir, "item-retry")?.join("people.json");
        assert!(
            relation_path.exists(),
            "the relation snapshot survives DB failure"
        );
        assert!(
            !service
                .item_actor_relation_is_current("item-retry", &source_fingerprint)
                .await?,
            "the checksum mismatch must make the partially persisted revision stale"
        );

        sqlx::query("DROP TRIGGER reject_relation_credit_state")
            .execute(database.pool())
            .await?;
        assert!(
            service
                .flush_deferred_nfo_actor_credits(&deferred_credits)
                .await
                .is_empty(),
            "the same deferred snapshot should be retryable after the storage fault clears"
        );
        assert!(
            service
                .flush_deferred_nfo_actor_credits(&deferred_credits)
                .await
                .is_empty(),
            "a repeated flush after success should be an idempotent no-op"
        );
        assert!(
            service
                .item_actor_relation_is_current("item-retry", &source_fingerprint)
                .await?
        );
        assert_relation_checksum_matches_file(&database, &config.config_dir, "item-retry").await?;

        service
            .persist_nfo_item_actors_deferred(
                "item-retry",
                "tmdb",
                &actors,
                &source_fingerprint,
                &deferred_credits,
            )
            .await?;
        assert!(
            service
                .flush_deferred_nfo_actor_credits(&deferred_credits)
                .await
                .is_empty(),
            "replaying the same relation update should remain idempotent"
        );
        let credit_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-retry'")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(credit_count, 1);
        assert_relation_checksum_matches_file(&database, &config.config_dir, "item-retry").await?;

        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn deferred_relation_credit_failure_recovers_from_snapshot_after_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{config::Config, storage::Database};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-restart-recovery', ?, 'MOVIE', 'Restart', 'Restart', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        let actors = [ActorCredit {
            id: "actor-restart".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: vec![PersonIdentity {
                provider: "tmdb".to_owned(),
                id: "actor-restart".to_owned(),
            }],
            name: "演员乙".to_owned(),
            character: Some("角色乙".to_owned()),
            order: Some(0),
            profile_url: None,
            person: None,
        }];
        let source_fingerprint = [17_u8; 32];
        let deferred_credits = DeferredNfoActorCredits::default();
        service
            .persist_nfo_item_actors_deferred(
                "item-restart-recovery",
                "tmdb",
                &actors,
                &source_fingerprint,
                &deferred_credits,
            )
            .await?;

        sqlx::query(
            "CREATE TRIGGER reject_restart_relation_credit_state
             BEFORE INSERT ON person_index_item_state
             WHEN NEW.item_id = 'item-restart-recovery'
             BEGIN SELECT RAISE(ABORT, 'injected relation state failure'); END",
        )
        .execute(database.pool())
        .await?;
        let failures = service
            .flush_deferred_nfo_actor_credits(&deferred_credits)
            .await;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].item_ids, ["item-restart-recovery"]);

        let relation_path = library_item_directory(&config.config_dir, "item-restart-recovery")?
            .join("people.json");
        let relation_bytes = tokio::fs::read(&relation_path).await?;
        let relation_checksum = relation_snapshot_checksum(&relation_bytes);
        assert!(
            !service
                .nfo_relation_snapshot_is_current("item-restart-recovery")
                .await?,
            "the durable relation snapshot must remain stale until its credit/index transaction commits"
        );

        // Lose the page-local retry queue and all service state as a process restart would.
        drop(deferred_credits);
        drop(service);
        database.close().await;

        let restarted_database = Database::connect(&config).await?;
        sqlx::query("DROP TRIGGER reject_restart_relation_credit_state")
            .execute(restarted_database.pool())
            .await?;
        let restarted_relation_bytes = tokio::fs::read(&relation_path).await?;
        assert_eq!(
            relation_snapshot_checksum(&restarted_relation_bytes),
            relation_checksum,
            "the recovery snapshot must survive the service and database restart"
        );
        let restarted_service =
            PeopleService::new(config.config_dir.clone()).with_database(restarted_database.clone());
        assert!(
            !restarted_service
                .nfo_relation_snapshot_is_current("item-restart-recovery")
                .await?,
            "a new service instance must detect the persisted snapshot/index mismatch"
        );

        assert_eq!(restarted_service.rebuild_person_credit_index().await?, 1);
        assert!(
            restarted_service
                .nfo_relation_snapshot_is_current("item-restart-recovery")
                .await?,
            "the existing person index rebuild must reconcile the durable snapshot"
        );
        let stored_checksum: String = sqlx::query_scalar(
            "SELECT relation_checksum FROM person_index_item_state
             WHERE item_id = 'item-restart-recovery'",
        )
        .fetch_one(restarted_database.pool())
        .await?;
        assert_eq!(stored_checksum, relation_checksum);
        let credit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM person_credits WHERE item_id = 'item-restart-recovery'",
        )
        .fetch_one(restarted_database.pool())
        .await?;
        assert_eq!(credit_count, 1);

        restarted_database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn deferred_nfo_person_manifests_share_one_restore_pending_write()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{config::Config, storage::Database};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let service = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
        let deferred_credits = DeferredNfoActorCredits::default();
        let identities_a = [PersonIdentity {
            provider: "tmdb".to_owned(),
            id: "person-a".to_owned(),
        }];
        let identities_b = [PersonIdentity {
            provider: "tmdb".to_owned(),
            id: "person-b".to_owned(),
        }];
        let person_dir_a = lux_person_directory(&config.config_dir, "演员甲", "lux-000001")?;
        let person_dir_b = lux_person_directory(&config.config_dir, "演员乙", "lux-000002")?;
        let actor_a = ActorCredit {
            id: "person-a".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: identities_a.to_vec(),
            name: "演员甲".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: None,
        };
        let actor_b = ActorCredit {
            id: "person-b".to_owned(),
            provider: Some("tmdb".to_owned()),
            identities: identities_b.to_vec(),
            name: "演员乙".to_owned(),
            character: None,
            order: Some(1),
            profile_url: None,
            person: None,
        };

        database.reset_query_count();
        let (result_a, result_b) = tokio::join!(
            service.persist_person_assets_with_deferred_manifest(
                &actor_a,
                "tmdb",
                "person-a",
                Some("lux-000001"),
                &identities_a,
                Some(&deferred_credits),
            ),
            service.persist_person_assets_with_deferred_manifest(
                &actor_b,
                "tmdb",
                "person-b",
                Some("lux-000002"),
                &identities_b,
                Some(&deferred_credits),
            )
        );
        assert!(
            !result_a
                .pending_assets
                .iter()
                .any(|asset| asset == PENDING_PERSON_MANIFEST)
        );
        assert!(
            !result_b
                .pending_assets
                .iter()
                .any(|asset| asset == PENDING_PERSON_MANIFEST)
        );

        assert_eq!(database.query_count(), 1);
        assert!(person_dir_a.join(PERSON_MANIFEST).is_file());
        assert!(person_dir_b.join(PERSON_MANIFEST).is_file());
        let manifest_a: PersonManifest =
            serde_json::from_slice(&tokio::fs::read(person_dir_a.join(PERSON_MANIFEST)).await?)?;
        let manifest_b: PersonManifest =
            serde_json::from_slice(&tokio::fs::read(person_dir_b.join(PERSON_MANIFEST)).await?)?;
        assert_eq!(manifest_a.identities, identities_a);
        assert_eq!(manifest_b.identities, identities_b);
        assert!(!manifest_a.checksum.is_empty());
        assert!(!manifest_b.checksum.is_empty());

        database.reset_query_count();
        service
            .persist_person_manifest(
                &person_dir_a,
                "lux-000001",
                "演员甲",
                "tmdb",
                &identities_a,
                PersonManifestWriteOptions {
                    metadata: None,
                    deferred_restore_pending: Some(&DeferredNfoActorCredits::default()),
                },
            )
            .await?;
        assert_eq!(
            database.query_count(),
            0,
            "unchanged manifest checksums must not mark restore pending"
        );

        database.close().await;
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

    #[tokio::test]
    async fn person_field_locks_are_versioned_and_recoverable()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = tempfile::tempdir()?;
        let service = PeopleService::new(config.path().to_owned());
        let person_dir = lux_person_directory(config.path(), "演员甲", "lux-000001")?;
        tokio::fs::create_dir_all(&person_dir).await?;
        let manifest = PersonManifest {
            schema_version: PERSON_MANIFEST_SCHEMA_VERSION,
            generation: 1,
            lux_person_id: "lux-000001".to_owned(),
            display_name: "演员甲".to_owned(),
            aliases: BTreeSet::new(),
            identities: Vec::new(),
            person: Some(PersonMetadata {
                biography: Some("本地简介".to_owned()),
                ..PersonMetadata::default()
            }),
            field_sources: BTreeMap::new(),
            locked_fields: BTreeSet::new(),
            identity_events: Vec::new(),
            metadata_events: Vec::new(),
            checksum: String::new(),
        };
        let mut manifest_bytes = serde_json::to_vec(&manifest)?;
        let mut signed = manifest.clone();
        signed.checksum = Sha256::digest(&manifest_bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        manifest_bytes = serde_json::to_vec(&signed)?;
        tokio::fs::write(person_dir.join(PERSON_MANIFEST), manifest_bytes).await?;
        let fields = service
            .set_person_field_locks(
                "lux-000001",
                &["biography".to_owned(), "name".to_owned()],
                r#"{"source":"test"}"#,
            )
            .await?;
        assert_eq!(fields, vec!["biography".to_owned(), "name".to_owned()]);
        let saved: PersonManifest =
            serde_json::from_slice(&tokio::fs::read(person_dir.join(PERSON_MANIFEST)).await?)?;
        assert_eq!(saved.generation, 2);
        assert_eq!(saved.locked_fields, fields.into_iter().collect());
        assert_eq!(
            saved.person.and_then(|person| person.biography),
            Some("本地简介".to_owned())
        );
        assert_eq!(saved.metadata_events.len(), 1);
        Ok(())
    }
}
