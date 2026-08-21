use std::{collections::HashSet, fmt, path::PathBuf, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

use crate::{
    application::emby_migration::{
        EmbyMigrationPluginClient, EmbyMigrationSource, HistoryCapability, MigrationConnectionInfo,
        MigrationInputError, MigrationItem, MigrationMergePolicy, MigrationUser, MigrationUserData,
        StoredItemState, merge_item_state,
    },
    application::plugins::PluginServiceError,
    auth::users::{UserStore, UserStoreError, UserUpdate},
    storage::{
        Database, StorageError, StoredEmbyMigrationJob, StoredEmbyMigrationSource,
        StoredEmbyMigrationUserBinding, StoredEmbyMigrationUserLink, StoredMigrationMediaIdentity,
    },
};

const SECRET_DIRECTORY: &str = "plugin-secrets/emby-migration";
const MAX_LABEL_LENGTH: usize = 128;
const MAX_JOB_PAGE_SIZE: i64 = 100;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateMigrationRequest {
    pub source: EmbyMigrationSource,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub merge_policy: MigrationMergePolicy,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationJobView {
    pub id: String,
    pub source_label: String,
    pub source_base_url: String,
    pub status: String,
    pub phase: String,
    pub dry_run: bool,
    pub merge_policy: String,
    pub processed_count: i64,
    pub total_count: i64,
    pub matched_count: i64,
    pub skipped_count: i64,
    pub failed_count: i64,
    pub cancel_requested: bool,
    pub error: Option<String>,
}

impl From<StoredEmbyMigrationJob> for MigrationJobView {
    fn from(job: StoredEmbyMigrationJob) -> Self {
        Self {
            id: job.id,
            source_label: job.source_label,
            source_base_url: job.source_base_url,
            status: job.status,
            phase: job.phase,
            dry_run: job.dry_run,
            merge_policy: job.merge_policy,
            processed_count: job.processed_count,
            total_count: job.total_count,
            matched_count: job.matched_count,
            skipped_count: job.skipped_count,
            failed_count: job.failed_count,
            cancel_requested: job.cancel_requested,
            error: job.error,
        }
    }
}

#[derive(Debug)]
pub enum EmbyMigrationServiceError {
    InvalidInput(MigrationInputError),
    Plugin(PluginServiceError),
    Storage(StorageError),
    User(UserStoreError),
    Io(std::io::Error),
    NotFound,
    InvalidState,
}

impl fmt::Display for EmbyMigrationServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(error) => error.fmt(formatter),
            Self::Plugin(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::User(error) => error.fmt(formatter),
            Self::Io(error) => write!(formatter, "migration secret storage failed: {error}"),
            Self::NotFound => formatter.write_str("migration job not found"),
            Self::InvalidState => formatter.write_str("migration job is not resumable"),
        }
    }
}

impl std::error::Error for EmbyMigrationServiceError {}

impl From<MigrationInputError> for EmbyMigrationServiceError {
    fn from(error: MigrationInputError) -> Self {
        Self::InvalidInput(error)
    }
}

impl From<PluginServiceError> for EmbyMigrationServiceError {
    fn from(error: PluginServiceError) -> Self {
        Self::Plugin(error)
    }
}

impl From<StorageError> for EmbyMigrationServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<UserStoreError> for EmbyMigrationServiceError {
    fn from(error: UserStoreError) -> Self {
        Self::User(error)
    }
}

impl From<std::io::Error> for EmbyMigrationServiceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone)]
pub struct EmbyMigrationService {
    database: Database,
    plugin: EmbyMigrationPluginClient,
    config_dir: PathBuf,
}

impl EmbyMigrationService {
    pub fn new(
        database: Database,
        plugins: crate::application::plugins::PluginService,
        config_dir: PathBuf,
    ) -> Self {
        Self {
            database,
            plugin: EmbyMigrationPluginClient::new(plugins),
            config_dir,
        }
    }

    pub async fn create_job(
        &self,
        created_by_user_id: &str,
        request: CreateMigrationRequest,
    ) -> Result<MigrationJobView, EmbyMigrationServiceError> {
        let source_url = request.source.validate()?;
        let source_base_url = source_url.to_string();
        let source_label = source_url
            .host_str()
            .ok_or(MigrationInputError::InvalidSourceUrl)?
            .to_owned();
        if source_label.chars().count() > MAX_LABEL_LENGTH {
            return Err(MigrationInputError::InvalidSourceUrl.into());
        }
        let job_id = Uuid::now_v7().to_string();
        let secret_ref = format!("emby-migration/{job_id}.json");
        self.write_secret(&secret_ref, &request.source).await?;
        let source = StoredEmbyMigrationSource {
            source_base_url: source_base_url.clone(),
            secret_ref: secret_ref.clone(),
            source_label: source_label.clone(),
            history_capability: "ITEM_STATE".to_owned(),
        };
        if let Err(error) = self.database.upsert_emby_migration_source(&source).await {
            let _ = self.remove_secret(&secret_ref).await;
            return Err(error.into());
        }
        if let Err(error) = self
            .database
            .insert_emby_migration_job(
                &job_id,
                created_by_user_id,
                &source_label,
                &source_base_url,
                &secret_ref,
                request.dry_run,
                merge_policy_name(request.merge_policy),
            )
            .await
        {
            let _ = self.remove_secret(&secret_ref).await;
            return Err(error.into());
        }
        self.get_job(&job_id).await
    }

    pub async fn get_job(
        &self,
        job_id: &str,
    ) -> Result<MigrationJobView, EmbyMigrationServiceError> {
        self.database
            .find_emby_migration_job(job_id)
            .await?
            .map(MigrationJobView::from)
            .ok_or(EmbyMigrationServiceError::NotFound)
    }

    pub async fn list_jobs(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<MigrationJobView>, EmbyMigrationServiceError> {
        let limit = limit.clamp(1, MAX_JOB_PAGE_SIZE);
        Ok(self
            .database
            .list_emby_migration_jobs(offset.max(0), limit)
            .await?
            .into_iter()
            .map(MigrationJobView::from)
            .collect())
    }

    pub async fn count_jobs(&self) -> Result<i64, EmbyMigrationServiceError> {
        Ok(self.database.count_emby_migration_jobs().await?)
    }

    pub async fn cancel_job(&self, job_id: &str) -> Result<bool, EmbyMigrationServiceError> {
        Ok(self.database.request_emby_migration_cancel(job_id).await?)
    }

    pub async fn resume_job(&self, job_id: &str) -> Result<bool, EmbyMigrationServiceError> {
        let job = self
            .database
            .find_emby_migration_job(job_id)
            .await?
            .ok_or(EmbyMigrationServiceError::NotFound)?;
        if !matches!(job.status.as_str(), "PENDING" | "RUNNING" | "FAILED") {
            return Err(EmbyMigrationServiceError::InvalidState);
        }
        if job.status == "FAILED" {
            self.database
                .update_emby_migration_job_status(job_id, "PENDING", &job.phase, None)
                .await?;
        }
        Ok(true)
    }

    pub async fn test_connection(
        &self,
        source: &EmbyMigrationSource,
    ) -> Result<MigrationConnectionInfo, EmbyMigrationServiceError> {
        Ok(self.plugin.test_connection(source).await?)
    }

    pub async fn authenticate_pending_user(
        &self,
        username: &str,
        password: &str,
    ) -> Result<bool, EmbyMigrationServiceError> {
        let Some(binding) = self
            .database
            .find_emby_migration_user_binding_by_username(username)
            .await?
        else {
            return Ok(false);
        };
        let secret_ref = if let Some(secret_ref) = binding.secret_ref.clone() {
            secret_ref
        } else {
            self.database
                .find_emby_migration_source(&binding.source_base_url)
                .await?
                .ok_or(EmbyMigrationServiceError::InvalidState)?
                .secret_ref
        };
        let path = self.config_dir.join("plugin-secrets").join(secret_ref);
        let contents = fs::read(path).await?;
        let source: EmbyMigrationSource = serde_json::from_slice(&contents)
            .map_err(|_| EmbyMigrationServiceError::InvalidState)?;
        let authenticated = self
            .plugin
            .authenticate_user(&source, username, password)
            .await?;
        if !authenticated.authenticated
            || authenticated.user_id.as_deref() != Some(binding.emby_user_id.as_str())
        {
            return Ok(false);
        }
        let user_store = UserStore::new(self.database.clone()).map_err(UserStoreError::from)?;
        user_store
            .update_user(
                &binding.lux_user_id,
                UserUpdate {
                    password: Some(password),
                    ..UserUpdate::default()
                },
            )
            .await?
            .ok_or(EmbyMigrationServiceError::NotFound)?;
        self.database
            .mark_emby_migration_password_ready(&binding.lux_user_id)
            .await?;
        Ok(true)
    }

    pub fn spawn(self: Arc<Self>, job_id: String) {
        tokio::spawn(async move {
            if let Err(error) = self.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "Emby migration job stopped");
            }
        });
    }

    async fn write_secret(
        &self,
        secret_ref: &str,
        source: &EmbyMigrationSource,
    ) -> Result<(), EmbyMigrationServiceError> {
        let directory = self.config_dir.join(SECRET_DIRECTORY);
        fs::create_dir_all(&directory).await?;
        let relative_path = PathBuf::from(secret_ref);
        let path = self.config_dir.join("plugin-secrets").join(&relative_path);
        let temporary = path.with_extension(format!("tmp-{}", Uuid::now_v7()));
        let contents = serde_json::to_vec(&json!({
            "baseUrl": source.base_url,
            "apiKey": source.api_key,
            "allowPrivateNetwork": source.allow_private_network,
        }))
        .map_err(|_| EmbyMigrationServiceError::InvalidState)?;
        let mut file = fs::File::create(&temporary).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = file.metadata().await?.permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(&temporary, permissions).await?;
        }
        file.write_all(&contents).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, &path).await?;
        Ok(())
    }

    async fn remove_secret(&self, secret_ref: &str) -> Result<(), std::io::Error> {
        let path = self.config_dir.join("plugin-secrets").join(secret_ref);
        match fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn run(&self, job_id: &str) -> Result<(), EmbyMigrationServiceError> {
        let Some(job) = self.database.find_emby_migration_job(job_id).await? else {
            return Err(EmbyMigrationServiceError::NotFound);
        };
        if matches!(job.status.as_str(), "COMPLETED" | "CANCELLED") {
            return Ok(());
        }
        let source = self.read_source(&job).await?;
        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "TESTING", None)
            .await?;
        let connection = match self.plugin.test_connection(&source).await {
            Ok(connection) => connection,
            Err(error) => {
                self.fail_job(job_id, "TESTING", &error.to_string()).await?;
                return Err(error.into());
            }
        };
        self.database
            .upsert_emby_migration_source(&StoredEmbyMigrationSource {
                source_base_url: job.source_base_url.clone(),
                secret_ref: job.secret_ref.clone(),
                source_label: job.source_label.clone(),
                history_capability: history_capability_name(connection.history_capability)
                    .to_owned(),
            })
            .await?;
        if self.is_cancelled(job_id).await? {
            return self.cancelled(job_id, "TESTING").await;
        }

        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "USERS", None)
            .await?;
        let users = match self.plugin.list_users(&source).await {
            Ok(page) => page.items,
            Err(error) => {
                self.fail_job(job_id, "USERS", &error.to_string()).await?;
                return Err(error.into());
            }
        };
        let user_store = UserStore::new(self.database.clone()).map_err(UserStoreError::from)?;
        let mut user_links = Vec::with_capacity(users.len());
        for user in &users {
            if self.is_cancelled(job_id).await? {
                return self.cancelled(job_id, "USERS").await;
            }
            let link = self.prepare_user(&user_store, &job, user).await?;
            self.database.upsert_emby_migration_user_link(&link).await?;
            user_links.push((user.clone(), link.lux_user_id.clone()));
        }

        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "ITEMS", None)
            .await?;
        let identities = self.load_media_identities().await?;
        let mut processed = job.processed_count;
        let mut matched = job.matched_count;
        let mut skipped = job.skipped_count;
        let failed = job.failed_count;
        let mut total = job.total_count.max(users.len() as i64);
        for (user, lux_user_id) in user_links {
            let mut start_index = 0_u32;
            let mut accessible_library_ids = HashSet::new();
            loop {
                if self.is_cancelled(job_id).await? {
                    return self.cancelled(job_id, "ITEMS").await;
                }
                let page = match self
                    .plugin
                    .user_state(&source, &user.id, start_index, 500)
                    .await
                {
                    Ok(page) => page,
                    Err(error) => {
                        self.fail_job(job_id, "ITEMS", &error.to_string()).await?;
                        return Err(error.into());
                    }
                };
                if let Some(page_total) = page.total_record_count {
                    total = total.max(page_total as i64);
                }
                for item in page.items {
                    let Some(user_data) = item.user_data.clone() else {
                        continue;
                    };
                    processed += 1;
                    let outcome = match_item(&item, &identities);
                    let detail = serde_json::to_string(&json!({
                        "sourceTitle": item.name,
                        "sourceType": item.item_type,
                        "productionYear": item.production_year,
                    }))
                    .unwrap_or_else(|_| "{}".to_owned());
                    self.database
                        .upsert_emby_migration_item_match(
                            job_id,
                            &item.id,
                            &item.item_type,
                            outcome.lux_item_id.as_deref(),
                            outcome.method,
                            outcome.confidence,
                            outcome.status,
                            &detail,
                        )
                        .await?;
                    let Some(lux_item_id) = outcome.lux_item_id else {
                        skipped += 1;
                        continue;
                    };
                    matched += 1;
                    if let Some(library_id) =
                        self.database.find_item_library_id(&lux_item_id).await?
                    {
                        accessible_library_ids.insert(library_id);
                    }
                    if job.dry_run {
                        continue;
                    }
                    let Some(lux_user_id) = lux_user_id.as_deref() else {
                        skipped += 1;
                        continue;
                    };
                    let incoming = incoming_state(&user_data)?;
                    let existing = self
                        .database
                        .find_user_item_state_for_migration(lux_user_id, &lux_item_id)
                        .await?
                        .map(|state| StoredItemState {
                            position_ticks: state.position_ticks,
                            is_played: state.is_played,
                            is_favorite: state.is_favorite,
                            play_count: state.play_count,
                            last_played_at: state.last_played_at,
                        });
                    let merged = merge_item_state(
                        existing,
                        incoming,
                        migration_merge_policy(&job.merge_policy),
                    )
                    .ok_or(EmbyMigrationServiceError::InvalidState)?;
                    self.database
                        .upsert_imported_user_item_state(
                            lux_user_id,
                            &lux_item_id,
                            merged.position_ticks,
                            merged.is_played,
                            merged.is_favorite,
                            merged.play_count,
                            merged.last_played_at,
                        )
                        .await?;
                    let state_hash = hex_sha256(&user_data)?;
                    self.database
                        .upsert_emby_migration_import_record(
                            job_id,
                            &user.id,
                            &item.id,
                            lux_user_id,
                            &lux_item_id,
                            &state_hash,
                            "IMPORTED",
                            None,
                        )
                        .await?;
                }
                self.database
                    .update_emby_migration_job_progress(
                        job_id,
                        &serde_json::to_string(&json!({
                            "userId": user.id,
                            "startIndex": page.start_index,
                        }))
                        .unwrap_or_else(|_| "{}".to_owned()),
                        processed,
                        total,
                        matched,
                        skipped,
                        failed,
                    )
                    .await?;
                let Some(next_start_index) = page.next_start_index else {
                    break;
                };
                if next_start_index <= start_index {
                    break;
                }
                start_index = next_start_index;
            }
            if !job.dry_run {
                let library_ids = if user.enable_all_folders {
                    self.database.list_enabled_library_ids().await?
                } else {
                    accessible_library_ids.into_iter().collect()
                };
                for library_id in library_ids {
                    if let Some(lux_user_id) = lux_user_id.as_deref() {
                        self.database
                            .set_user_library_access(lux_user_id, &library_id, true)
                            .await?;
                    }
                }
            }
        }
        self.database
            .update_emby_migration_job_status(job_id, "RUNNING", "FINALIZING", None)
            .await?;
        self.database
            .update_emby_migration_job_progress(
                job_id, "{}", processed, total, matched, skipped, failed,
            )
            .await?;
        self.database
            .update_emby_migration_job_status(job_id, "COMPLETED", "FINALIZING", None)
            .await?;
        Ok(())
    }

    async fn read_source(
        &self,
        job: &StoredEmbyMigrationJob,
    ) -> Result<EmbyMigrationSource, EmbyMigrationServiceError> {
        let path = self.config_dir.join("plugin-secrets").join(&job.secret_ref);
        let contents = fs::read(path).await?;
        serde_json::from_slice(&contents).map_err(|_| EmbyMigrationServiceError::InvalidState)
    }

    async fn prepare_user(
        &self,
        user_store: &UserStore,
        job: &StoredEmbyMigrationJob,
        source_user: &MigrationUser,
    ) -> Result<StoredEmbyMigrationUserLink, EmbyMigrationServiceError> {
        let source_user_name = source_user.name.trim();
        if source_user_name.is_empty() {
            return Ok(StoredEmbyMigrationUserLink {
                job_id: job.id.clone(),
                emby_user_id: source_user.id.clone(),
                emby_username: source_user.name.clone(),
                lux_user_id: None,
                status: "SKIPPED".to_owned(),
                error: Some("empty Emby username".to_owned()),
            });
        }
        if job.dry_run {
            return Ok(StoredEmbyMigrationUserLink {
                job_id: job.id.clone(),
                emby_user_id: source_user.id.clone(),
                emby_username: source_user_name.to_owned(),
                lux_user_id: None,
                status: "SKIPPED".to_owned(),
                error: Some("DRY_RUN".to_owned()),
            });
        }
        let existing = user_store.find_by_username(source_user_name).await?;
        let (lux_user, status) = match existing {
            Some(user) => {
                if user.is_disabled != source_user.is_disabled {
                    user_store
                        .update_user(
                            &user.id.to_string(),
                            UserUpdate {
                                is_disabled: Some(source_user.is_disabled),
                                ..UserUpdate::default()
                            },
                        )
                        .await?;
                }
                (user, "LINKED")
            }
            None => {
                let placeholder = Uuid::now_v7().to_string();
                let user = user_store
                    .create_user(source_user_name, source_user_name, &placeholder, false)
                    .await?;
                let user = user_store
                    .update_user(
                        &user.id.to_string(),
                        UserUpdate {
                            is_disabled: Some(source_user.is_disabled),
                            ..UserUpdate::default()
                        },
                    )
                    .await?
                    .ok_or(EmbyMigrationServiceError::NotFound)?;
                (user, "AUTO_CREATED")
            }
        };
        self.database
            .upsert_emby_migration_user_binding(&StoredEmbyMigrationUserBinding {
                lux_user_id: lux_user.id.to_string(),
                source_base_url: job.source_base_url.clone(),
                secret_ref: Some(job.secret_ref.clone()),
                emby_user_id: source_user.id.clone(),
                emby_username: source_user_name.to_owned(),
                password_pending: source_user.has_password && !source_user.is_disabled,
            })
            .await?;
        Ok(StoredEmbyMigrationUserLink {
            job_id: job.id.clone(),
            emby_user_id: source_user.id.clone(),
            emby_username: source_user_name.to_owned(),
            lux_user_id: Some(lux_user.id.to_string()),
            status: status.to_owned(),
            error: None,
        })
    }

    async fn load_media_identities(
        &self,
    ) -> Result<Vec<StoredMigrationMediaIdentity>, EmbyMigrationServiceError> {
        let mut identities = Vec::new();
        let mut after_id = None;
        loop {
            let page = self
                .database
                .list_migration_media_identities(after_id.as_deref(), 500)
                .await?;
            if page.is_empty() {
                break;
            }
            after_id = page.last().map(|item| item.id.clone());
            let page_len = page.len();
            identities.extend(page);
            if page_len < 500 {
                break;
            }
        }
        Ok(identities)
    }

    async fn is_cancelled(&self, job_id: &str) -> Result<bool, EmbyMigrationServiceError> {
        Ok(self
            .database
            .find_emby_migration_job(job_id)
            .await?
            .is_some_and(|job| job.cancel_requested))
    }

    async fn cancelled(&self, job_id: &str, phase: &str) -> Result<(), EmbyMigrationServiceError> {
        self.database
            .update_emby_migration_job_status(job_id, "CANCELLED", phase, None)
            .await?;
        Ok(())
    }

    async fn fail_job(
        &self,
        job_id: &str,
        phase: &str,
        error: &str,
    ) -> Result<(), EmbyMigrationServiceError> {
        self.database
            .update_emby_migration_job_status(job_id, "FAILED", phase, Some(error))
            .await?;
        Ok(())
    }
}

struct MatchOutcome {
    lux_item_id: Option<String>,
    method: &'static str,
    confidence: Option<i64>,
    status: &'static str,
}

fn match_item(item: &MigrationItem, identities: &[StoredMigrationMediaIdentity]) -> MatchOutcome {
    let expected_type = match item.item_type.as_str() {
        "Movie" => "MOVIE",
        "Series" => "SERIES",
        "Season" => "SEASON",
        "Episode" => "EPISODE",
        _ => return unmatched("unsupported item type"),
    };
    let mut provider_matches = Vec::new();
    for identity in identities
        .iter()
        .filter(|identity| identity.item_type == expected_type)
    {
        let Some(provider_ids_json) = identity.provider_ids_json.as_deref() else {
            continue;
        };
        let Ok(provider_ids) =
            serde_json::from_str::<std::collections::BTreeMap<String, String>>(provider_ids_json)
        else {
            continue;
        };
        if item.provider_ids.iter().any(|(source_key, source_value)| {
            provider_ids.iter().any(|(target_key, target_value)| {
                source_key.eq_ignore_ascii_case(target_key) && source_value == target_value
            })
        }) {
            provider_matches.push(identity.id.clone());
        }
    }
    if provider_matches.len() == 1 {
        let method = if item
            .provider_ids
            .keys()
            .any(|key| key.eq_ignore_ascii_case("Tmdb"))
        {
            "TMDB_ID"
        } else {
            "PROVIDER_ID"
        };
        return MatchOutcome {
            lux_item_id: provider_matches.into_iter().next(),
            method,
            confidence: Some(100),
            status: "MATCHED",
        };
    }
    if provider_matches.len() > 1 {
        return MatchOutcome {
            lux_item_id: None,
            method: "CONFLICT",
            confidence: None,
            status: "CONFLICT",
        };
    }

    let title = normalize_title(&item.name);
    if title.is_empty() {
        return unmatched("empty title");
    }
    let mut title_matches = identities
        .iter()
        .filter(|identity| identity.item_type == expected_type)
        .filter(|identity| normalize_title(&identity.title) == title)
        .filter(
            |identity| match (item.production_year, identity.production_year) {
                (Some(source_year), Some(target_year)) => (source_year - target_year).abs() <= 1,
                _ => true,
            },
        )
        .filter(|identity| {
            if expected_type != "EPISODE" {
                return true;
            }
            item.index_number
                .is_none_or(|number| identity.episode_number == Some(number))
                && item
                    .parent_index_number
                    .is_none_or(|number| identity.season_number == Some(number))
        })
        .map(|identity| identity.id.clone())
        .collect::<Vec<_>>();
    if title_matches.len() == 1 {
        return MatchOutcome {
            lux_item_id: title_matches.pop(),
            method: if expected_type == "EPISODE" {
                "EPISODE_KEY"
            } else {
                "TITLE_YEAR"
            },
            confidence: Some(if expected_type == "EPISODE" { 95 } else { 90 }),
            status: "MATCHED",
        };
    }
    if title_matches.len() > 1 {
        return MatchOutcome {
            lux_item_id: None,
            method: "CONFLICT",
            confidence: None,
            status: "CONFLICT",
        };
    }
    unmatched("no unique media match")
}

fn unmatched(_reason: &str) -> MatchOutcome {
    MatchOutcome {
        lux_item_id: None,
        method: "UNMATCHED",
        confidence: None,
        status: "UNMATCHED",
    }
}

fn normalize_title(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn incoming_state(data: &MigrationUserData) -> Result<StoredItemState, EmbyMigrationServiceError> {
    let last_played_at = data
        .last_played_date
        .as_deref()
        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
        .map(|value| value.unix_timestamp());
    Ok(StoredItemState {
        position_ticks: data.playback_position_ticks,
        is_played: data.played,
        is_favorite: data.is_favorite,
        play_count: data.play_count,
        last_played_at,
    })
}

fn migration_merge_policy(value: &str) -> MigrationMergePolicy {
    match value {
        "OVERWRITE" => MigrationMergePolicy::Overwrite,
        "SKIP" => MigrationMergePolicy::Skip,
        _ => MigrationMergePolicy::Merge,
    }
}

fn hex_sha256(value: &MigrationUserData) -> Result<String, EmbyMigrationServiceError> {
    let bytes = serde_json::to_vec(value).map_err(|_| EmbyMigrationServiceError::InvalidState)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn merge_policy_name(policy: MigrationMergePolicy) -> &'static str {
    match policy {
        MigrationMergePolicy::Merge => "MERGE",
        MigrationMergePolicy::Overwrite => "OVERWRITE",
        MigrationMergePolicy::Skip => "SKIP",
    }
}

#[allow(dead_code)]
fn history_capability_name(capability: HistoryCapability) -> &'static str {
    match capability {
        HistoryCapability::ItemState => "ITEM_STATE",
        HistoryCapability::EventHistory => "EVENT_HISTORY",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn identity(id: &str, provider_ids: &str) -> StoredMigrationMediaIdentity {
        StoredMigrationMediaIdentity {
            id: id.to_owned(),
            item_type: "MOVIE".to_owned(),
            title: "The Film".to_owned(),
            production_year: Some(2024),
            provider_ids_json: Some(provider_ids.to_owned()),
            series_id: None,
            season_number: None,
            episode_number: None,
        }
    }

    #[test]
    fn provider_id_match_is_unique_and_strongest() {
        let item = MigrationItem {
            id: "emby-1".to_owned(),
            name: "Different title".to_owned(),
            item_type: "Movie".to_owned(),
            production_year: None,
            provider_ids: BTreeMap::from([(String::from("Tmdb"), String::from("42"))]),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: None,
        };
        let outcome = match_item(&item, &[identity("lux-1", r#"{"tmdb":"42"}"#)]);
        assert_eq!(outcome.lux_item_id.as_deref(), Some("lux-1"));
        assert_eq!(outcome.method, "TMDB_ID");
        assert_eq!(outcome.status, "MATCHED");
    }

    #[test]
    fn ambiguous_title_match_is_not_guessed() {
        let item = MigrationItem {
            id: "emby-1".to_owned(),
            name: "The Film".to_owned(),
            item_type: "Movie".to_owned(),
            production_year: Some(2024),
            provider_ids: BTreeMap::new(),
            parent_id: None,
            series_id: None,
            season_id: None,
            index_number: None,
            parent_index_number: None,
            user_data: None,
        };
        let outcome = match_item(&item, &[identity("lux-1", "{}"), identity("lux-2", "{}")]);
        assert_eq!(outcome.lux_item_id, None);
        assert_eq!(outcome.status, "CONFLICT");
    }

    #[test]
    fn invalid_last_played_date_is_non_fatal() {
        let data = MigrationUserData {
            playback_position_ticks: 10,
            played: true,
            is_favorite: false,
            play_count: 1,
            last_played_date: Some("not-a-date".to_owned()),
        };
        let state = incoming_state(&data).expect("valid numeric state should be accepted");
        assert_eq!(state.last_played_at, None);
        assert_eq!(state.position_ticks, 10);
    }
}
